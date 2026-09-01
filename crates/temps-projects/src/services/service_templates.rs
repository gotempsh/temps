// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-through catalog for Coolify's Apache-2.0 one-click service templates.
//!
//! The remote catalog is control-plane data: it is fetched only when a user
//! opens the service gallery, bounded before parsing, cached for one hour, and
//! copied into the project archive at install time. Existing projects never
//! change when the upstream catalog changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::Deserialize;
use serde::Deserializer;
use serde_yaml::{Mapping, Value as YamlValue};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, Semaphore};

pub const COOLIFY_CATALOG_URL: &str =
    "https://cdn.coollabs.io/coolify/service-templates-latest.json";
pub const COOLIFY_REPOSITORY_URL: &str = "https://github.com/coollabsio/coolify";

const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);
const FAILED_REFRESH_BACKOFF: Duration = Duration::from_secs(30);
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEMPLATE_COMPOSE_BYTES: usize = 512 * 1024;
const MAX_CATALOG_ENTRIES: usize = 2_000;
const MAX_PREFLIGHT_VARIABLES: usize = 256;
const MAX_PREFLIGHT_VALUE_BYTES: usize = 16 * 1024;
const MAX_PREFLIGHT_TOTAL_VALUE_BYTES: usize = 256 * 1024;
const MAX_CONCURRENT_PREFLIGHTS: usize = 4;
const SAFE_DOCKER_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin";
const DOCKER_BINARY_CANDIDATES: &[&str] = &[
    "/usr/local/bin/docker",
    "/usr/bin/docker",
    "/opt/homebrew/bin/docker",
];
static PREFLIGHT_COMPOSE_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_PREFLIGHTS);

#[derive(Debug, Clone, Error)]
pub enum ServiceTemplateCatalogError {
    #[error("Failed to build the Coolify catalog HTTP client: {reason}")]
    ClientBuild { reason: String },
    #[error("Failed to fetch Coolify service catalog from '{url}': {reason}")]
    Fetch { url: String, reason: String },
    #[error("Coolify service catalog at '{url}' returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("Coolify service catalog at '{url}' exceeded the {limit_bytes}-byte safety limit")]
    CatalogTooLarge { url: String, limit_bytes: usize },
    #[error("Coolify service catalog at '{url}' is invalid: {reason}")]
    InvalidCatalog { url: String, reason: String },
    #[error("Coolify service catalog contains {count} entries; the supported maximum is {limit}")]
    TooManyEntries { count: usize, limit: usize },
    #[error("Service template '{slug}' was not found in the Coolify catalog")]
    NotFound { slug: String },
    #[error("Service template '{slug}' has invalid base64 Compose content: {reason}")]
    InvalidComposeEncoding { slug: String, reason: String },
    #[error("Service template '{slug}' Compose content is not UTF-8: {reason}")]
    InvalidComposeText { slug: String, reason: String },
    #[error(
        "Service template '{slug}' Compose content exceeded the {limit_bytes}-byte safety limit"
    )]
    ComposeTooLarge { slug: String, limit_bytes: usize },
    #[error("Service template '{slug}' has invalid Compose YAML: {reason}")]
    InvalidComposeYaml { slug: String, reason: String },
    #[error("Service template '{slug}' preflight input is invalid: {reason}")]
    InvalidPreflightInput { slug: String, reason: String },
    #[error(
        "Service template '{slug}' preflight is busy; at most {limit} Compose validations may run concurrently"
    )]
    PreflightBusy { slug: String, limit: usize },
    #[error("Service template '{slug}' changed after it was opened; reload it before installing")]
    RevisionChanged { slug: String },
    #[error("Service template '{slug}' preflight infrastructure failed: {reason}")]
    PreflightInfrastructure { slug: String, reason: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyTemplate {
    pub documentation: Option<String>,
    pub slogan: Option<String>,
    pub compose: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub logo: Option<String>,
    pub port: Option<String>,
    pub template_last_updated_at: Option<String>,
    #[serde(default)]
    pub amd_only: bool,
    #[serde(default)]
    pub arm_only: bool,
}

#[derive(Debug)]
pub struct CatalogSnapshot {
    pub templates: BTreeMap<String, CoolifyTemplate>,
    pub analyses: BTreeMap<String, CatalogTemplateAnalysis>,
    pub fetched_at: DateTime<Utc>,
    pub etag: Option<String>,
    refreshed_at: Instant,
}

#[derive(Debug, Clone)]
pub struct CatalogTemplateAnalysis {
    pub service_count: usize,
    pub backing_services: Vec<TemplateBackingService>,
    pub installable: bool,
    pub compatibility_tier: TemplateCompatibilityTier,
    pub compatibility_issues: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TemplateBackingServiceKind {
    Postgres,
    Redis,
    MongoDb,
    S3,
}

impl TemplateBackingServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Redis => "redis",
            Self::MongoDb => "mongodb",
            Self::S3 => "s3",
        }
    }

    pub fn discovery_tag(self) -> &'static str {
        match self {
            Self::Postgres => "postgresql",
            Self::Redis => "redis",
            Self::MongoDb => "mongodb",
            Self::S3 => "s3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TemplateBackingService {
    pub service: String,
    pub kind: TemplateBackingServiceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateVariableKind {
    PublicUrl,
    PublicHost,
    GeneratedPassword,
    GeneratedPassword64,
    GeneratedPasswordWithSymbols,
    GeneratedPasswordWithSymbols64,
    GeneratedUser,
    GeneratedLowercaseUser,
    GeneratedRandom32,
    GeneratedRandom64,
    GeneratedRandom128,
    GeneratedBase64_32,
    GeneratedBase64_64,
    GeneratedBase64_128,
    GeneratedHex32,
    GeneratedHex64,
    GeneratedHex128,
    GeneratedSupabaseAnon,
    GeneratedSupabaseService,
    UserInput,
}

impl TemplateVariableKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PublicUrl => "public_url",
            Self::PublicHost => "public_host",
            Self::GeneratedPassword => "generated_password",
            Self::GeneratedPassword64 => "generated_password_64",
            Self::GeneratedPasswordWithSymbols => "generated_password_with_symbols",
            Self::GeneratedPasswordWithSymbols64 => "generated_password_with_symbols_64",
            Self::GeneratedUser => "generated_user",
            Self::GeneratedLowercaseUser => "generated_lowercase_user",
            Self::GeneratedRandom32 => "generated_random_32",
            Self::GeneratedRandom64 => "generated_random_64",
            Self::GeneratedRandom128 => "generated_random_128",
            Self::GeneratedBase64_32 => "generated_base64_32",
            Self::GeneratedBase64_64 => "generated_base64_64",
            Self::GeneratedBase64_128 => "generated_base64_128",
            Self::GeneratedHex32 => "generated_hex_32",
            Self::GeneratedHex64 => "generated_hex_64",
            Self::GeneratedHex128 => "generated_hex_128",
            Self::GeneratedSupabaseAnon => "generated_supabase_anon",
            Self::GeneratedSupabaseService => "generated_supabase_service",
            Self::UserInput => "user_input",
        }
    }

    pub fn is_secret(&self) -> bool {
        matches!(
            self,
            Self::GeneratedPassword
                | Self::GeneratedPassword64
                | Self::GeneratedPasswordWithSymbols
                | Self::GeneratedPasswordWithSymbols64
                | Self::GeneratedRandom32
                | Self::GeneratedRandom64
                | Self::GeneratedRandom128
                | Self::GeneratedBase64_32
                | Self::GeneratedBase64_64
                | Self::GeneratedBase64_128
                | Self::GeneratedHex32
                | Self::GeneratedHex64
                | Self::GeneratedHex128
                | Self::GeneratedSupabaseAnon
                | Self::GeneratedSupabaseService
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVariable {
    pub name: String,
    pub kind: TemplateVariableKind,
    pub required: bool,
    pub default_value: Option<String>,
    /// Compose service whose generated hostname backs a URL/FQDN variable.
    pub route_service: Option<String>,
}

impl TemplateVariable {
    pub fn is_secret(&self) -> bool {
        self.kind.is_secret() || temps_presets::compose_environment_name_is_secret(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRoute {
    pub service: String,
    pub port: u16,
    pub variable_names: Vec<String>,
    pub health_check_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateTransformation {
    pub code: &'static str,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCapabilityRequirement {
    pub service: String,
    pub capability: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateCompatibilityTier {
    Standard,
    Elevated,
    HostAccess,
    Blocked,
}

impl TemplateCompatibilityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Elevated => "elevated",
            Self::HostAccess => "host_access",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedServiceTemplate {
    pub compose: String,
    pub service_count: usize,
    pub backing_services: Vec<TemplateBackingService>,
    pub routes: Vec<TemplateRoute>,
    pub variables: Vec<TemplateVariable>,
    pub compatibility_issues: Vec<String>,
    pub warnings: Vec<String>,
    pub transformations: Vec<TemplateTransformation>,
    pub capability_requirements: Vec<TemplateCapabilityRequirement>,
    /// The template asks to cross the project sandbox into host-owned
    /// resources such as the Docker API, host namespaces, or devices.
    pub requires_host_access: bool,
}

#[derive(Debug, Clone)]
pub struct TemplatePreflightResult {
    pub compose_validated: bool,
    pub architecture: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl TemplatePreflightResult {
    pub fn ready(&self) -> bool {
        self.compose_validated && self.errors.is_empty()
    }
}

impl PreparedServiceTemplate {
    pub fn installable(&self) -> bool {
        self.compatibility_issues.is_empty()
    }

    pub fn compatibility_tier(&self) -> TemplateCompatibilityTier {
        if !self.installable() {
            if self.requires_host_access {
                TemplateCompatibilityTier::HostAccess
            } else {
                TemplateCompatibilityTier::Blocked
            }
        } else if self.capability_requirements.is_empty() {
            TemplateCompatibilityTier::Standard
        } else {
            TemplateCompatibilityTier::Elevated
        }
    }

    pub fn install_plan_digest(&self, template: &CoolifyTemplate) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut hasher = Sha256::new();
        digest_field(&mut hasher, b"temps-service-template-install-plan-v1");
        digest_field(&mut hasher, self.compose.as_bytes());
        digest_field(
            &mut hasher,
            template.port.as_deref().unwrap_or_default().as_bytes(),
        );
        digest_field(&mut hasher, &[u8::from(template.amd_only)]);
        digest_field(&mut hasher, &[u8::from(template.arm_only)]);
        for route in &self.routes {
            digest_field(&mut hasher, route.service.as_bytes());
            digest_field(&mut hasher, &route.port.to_be_bytes());
            digest_field(
                &mut hasher,
                route
                    .health_check_path
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
            for variable_name in &route.variable_names {
                digest_field(&mut hasher, variable_name.as_bytes());
            }
        }
        for backing_service in &self.backing_services {
            digest_field(&mut hasher, backing_service.kind.as_str().as_bytes());
            digest_field(&mut hasher, backing_service.service.as_bytes());
        }
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
        encoded
    }
}

fn digest_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub async fn preflight_template(
    slug: &str,
    template: &CoolifyTemplate,
    values: &BTreeMap<String, String>,
    approved_capability_services: &[String],
) -> Result<TemplatePreflightResult, ServiceTemplateCatalogError> {
    let prepared = prepare_template(slug, template)?;
    validate_preflight_values(slug, &prepared.variables, values)?;
    let architecture = host_architecture().to_string();
    let mut errors = prepared.compatibility_issues.clone();
    let mut warnings = prepared.warnings.clone();

    if template.amd_only && architecture != "amd64" {
        errors.push(format!(
            "Template '{slug}' requires amd64, but this Temps server is {architecture}"
        ));
    }
    if template.arm_only && architecture != "arm64" {
        errors.push(format!(
            "Template '{slug}' requires arm64, but this Temps server is {architecture}"
        ));
    }

    for variable in &prepared.variables {
        let supplied = values
            .get(&variable.name)
            .is_some_and(|value| !value.trim().is_empty());
        if variable.required && variable.default_value.is_none() && !supplied {
            errors.push(format!(
                "Required variable '{}' has no value",
                variable.name
            ));
        }
    }

    let approved = approved_capability_services
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for requirement in &prepared.capability_requirements {
        if !approved.contains(requirement.service.as_str()) {
            errors.push(format!(
                "Service '{}' requires confirmation of limited startup permissions: {}",
                requirement.service, requirement.reason
            ));
        }
    }
    for service in &approved {
        if !prepared
            .capability_requirements
            .iter()
            .any(|requirement| requirement.service == *service)
        {
            warnings.push(format!(
                "Startup permission confirmation for service '{service}' is unnecessary and will be ignored"
            ));
        }
    }

    errors.sort();
    errors.dedup();
    warnings.sort();
    warnings.dedup();
    if errors.is_empty() {
        if let Err(error) = temps_presets::validate_compose_credentials(&prepared.compose) {
            errors.push(error.to_string());
        }
    }
    let compose_validated = if errors.is_empty() {
        let _permit = PREFLIGHT_COMPOSE_SLOTS.try_acquire().map_err(|_| {
            ServiceTemplateCatalogError::PreflightBusy {
                slug: slug.to_string(),
                limit: MAX_CONCURRENT_PREFLIGHTS,
            }
        })?;
        match validate_with_docker_compose(slug, &prepared.compose, &prepared.variables, values)
            .await?
        {
            Ok(()) => true,
            Err(reason) => {
                errors.push(reason);
                false
            }
        }
    } else {
        false
    };

    Ok(TemplatePreflightResult {
        compose_validated,
        architecture,
        errors,
        warnings,
    })
}

fn validate_preflight_values(
    slug: &str,
    variables: &[TemplateVariable],
    values: &BTreeMap<String, String>,
) -> Result<(), ServiceTemplateCatalogError> {
    if values.len() > MAX_PREFLIGHT_VARIABLES {
        return Err(ServiceTemplateCatalogError::InvalidPreflightInput {
            slug: slug.to_string(),
            reason: format!(
                "received {} variables; the supported maximum is {MAX_PREFLIGHT_VARIABLES}",
                values.len()
            ),
        });
    }
    let declared = variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut total_bytes = 0_usize;
    for (name, value) in values {
        if !declared.contains(name.as_str()) {
            return Err(ServiceTemplateCatalogError::InvalidPreflightInput {
                slug: slug.to_string(),
                reason: format!("variable '{name}' is not declared by this template"),
            });
        }
        if value.len() > MAX_PREFLIGHT_VALUE_BYTES {
            return Err(ServiceTemplateCatalogError::InvalidPreflightInput {
                slug: slug.to_string(),
                reason: format!(
                    "variable '{name}' is {} bytes; the supported maximum is {MAX_PREFLIGHT_VALUE_BYTES}",
                    value.len()
                ),
            });
        }
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
    }
    if total_bytes > MAX_PREFLIGHT_TOTAL_VALUE_BYTES {
        return Err(ServiceTemplateCatalogError::InvalidPreflightInput {
            slug: slug.to_string(),
            reason: format!(
                "variable data is {total_bytes} bytes; the supported maximum is {MAX_PREFLIGHT_TOTAL_VALUE_BYTES}"
            ),
        });
    }
    Ok(())
}

fn host_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        architecture => architecture,
    }
}

async fn validate_with_docker_compose(
    slug: &str,
    compose: &str,
    variables: &[TemplateVariable],
    values: &BTreeMap<String, String>,
) -> Result<Result<(), String>, ServiceTemplateCatalogError> {
    let directory = tempfile::Builder::new()
        .prefix("temps-service-template-preflight-")
        .tempdir()
        .map_err(
            |error| ServiceTemplateCatalogError::PreflightInfrastructure {
                slug: slug.to_string(),
                reason: format!("could not create a temporary directory: {error}"),
            },
        )?;
    let compose_path = directory.path().join("docker-compose.yml");
    tokio::fs::write(&compose_path, compose)
        .await
        .map_err(
            |error| ServiceTemplateCatalogError::PreflightInfrastructure {
                slug: slug.to_string(),
                reason: format!(
                    "could not write temporary Compose file '{}': {error}",
                    compose_path.display()
                ),
            },
        )?;
    create_preflight_env_files(directory.path(), compose, slug)
        .await
        .map_err(
            |reason| ServiceTemplateCatalogError::PreflightInfrastructure {
                slug: slug.to_string(),
                reason,
            },
        )?;
    let env_content = render_preflight_env(variables, values).map_err(|reason| {
        ServiceTemplateCatalogError::InvalidPreflightInput {
            slug: slug.to_string(),
            reason,
        }
    })?;
    tokio::fs::write(directory.path().join(".env"), env_content)
        .await
        .map_err(
            |error| ServiceTemplateCatalogError::PreflightInfrastructure {
                slug: slug.to_string(),
                reason: format!("could not create temporary .env: {error}"),
            },
        )?;

    let mut command = isolated_docker_command();
    command
        .args([
            "compose",
            "--env-file",
            ".env",
            "-f",
            "docker-compose.yml",
            "config",
            "--quiet",
        ])
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| ServiceTemplateCatalogError::PreflightInfrastructure {
            slug: slug.to_string(),
            reason: "Docker Compose validation timed out after 15 seconds".to_string(),
        })?
        .map_err(
            |error| ServiceTemplateCatalogError::PreflightInfrastructure {
                slug: slug.to_string(),
                reason: format!("Docker Compose is unavailable: {error}"),
            },
        )?;
    if output.status.success() {
        return Ok(Ok(()));
    }
    let mut diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
    for value in values.values().filter(|value| !value.is_empty()) {
        diagnostic = diagnostic.replace(value, "***");
    }
    diagnostic.truncate(diagnostic.floor_char_boundary(8 * 1024));
    Ok(Err(format!(
        "Docker Compose rejected template '{slug}': {}",
        diagnostic.trim()
    )))
}

/// Create a Docker CLI process whose environment cannot expose Temps secrets
/// to Compose interpolation. The allowlist mirrors the deployer's command
/// isolation: only administrator-owned Docker connection configuration is
/// inherited, while template values are supplied through the bounded `.env`.
fn isolated_docker_command() -> tokio::process::Command {
    let docker_binary = DOCKER_BINARY_CANDIDATES
        .iter()
        .find(|candidate| Path::new(candidate).is_file())
        .copied()
        .unwrap_or("/usr/bin/docker");
    let mut command = tokio::process::Command::new(docker_binary);
    command.env_clear().env("PATH", SAFE_DOCKER_PATH);
    for key in [
        "HOME",
        "DOCKER_CONFIG",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

fn render_preflight_env(
    variables: &[TemplateVariable],
    values: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut rendered = String::new();
    for variable in variables {
        let Some(value) = values.get(&variable.name) else {
            continue;
        };
        let mut encoded = String::with_capacity(value.len() + 2);
        encoded.push('"');
        for character in value.chars() {
            match character {
                '\\' => encoded.push_str("\\\\"),
                '"' => encoded.push_str("\\\""),
                '\n' => encoded.push_str("\\n"),
                '\r' => encoded.push_str("\\r"),
                '\t' => encoded.push_str("\\t"),
                '$' => encoded.push_str("$$"),
                character if character.is_control() => {
                    return Err(format!(
                        "Variable '{}' contains unsupported control character U+{:04X}",
                        variable.name, character as u32
                    ));
                }
                _ => encoded.push(character),
            }
        }
        encoded.push('"');
        rendered.push_str(&variable.name);
        rendered.push('=');
        rendered.push_str(&encoded);
        rendered.push('\n');
    }
    Ok(rendered)
}

async fn create_preflight_env_files(
    directory: &Path,
    compose: &str,
    slug: &str,
) -> Result<(), String> {
    let root: YamlValue = serde_yaml::from_str(compose).map_err(|error| {
        format!("Could not inspect env_file entries for template '{slug}': {error}")
    })?;
    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return Ok(());
    };
    let mut paths = BTreeSet::new();
    for service in services.values().filter_map(YamlValue::as_mapping) {
        if let Some(env_file) = service.get("env_file") {
            paths.extend(env_file_paths(env_file).into_iter().map(str::to_string));
        }
    }
    for path in paths {
        if is_unsafe_volume_source(&path) {
            return Err(format!(
                "Template '{slug}' references unsafe env file path '{path}'"
            ));
        }
        let destination = directory.join(&path);
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                format!(
                    "Could not create preflight env file directory '{}' for template '{slug}': {error}",
                    parent.display()
                )
            })?;
        }
        tokio::fs::write(&destination, "").await.map_err(|error| {
            format!(
                "Could not create preflight env file '{}' for template '{slug}': {error}",
                destination.display()
            )
        })?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct ServiceTemplateCatalog {
    client: reqwest::Client,
    source_url: String,
    cache: RwLock<Option<Arc<CatalogSnapshot>>>,
    refresh_guard: Mutex<()>,
    last_failed_refresh: RwLock<Option<(Instant, ServiceTemplateCatalogError)>>,
}

impl ServiceTemplateCatalog {
    pub fn new() -> Result<Self, ServiceTemplateCatalogError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("temps/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ServiceTemplateCatalogError::ClientBuild {
                reason: error.to_string(),
            })?;
        Ok(Self::with_client(client, COOLIFY_CATALOG_URL.to_string()))
    }

    pub fn with_client(client: reqwest::Client, source_url: String) -> Self {
        Self {
            client,
            source_url,
            cache: RwLock::new(None),
            refresh_guard: Mutex::new(()),
            last_failed_refresh: RwLock::new(None),
        }
    }

    pub async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, ServiceTemplateCatalogError> {
        if let Some(snapshot) = self.fresh_snapshot().await {
            return Ok(snapshot);
        }

        let stale = self.cache.read().await.clone();
        if let Some(result) = self.backoff_result(stale.clone()).await {
            return result;
        }

        let _refresh_guard = self.refresh_guard.lock().await;
        if let Some(snapshot) = self.fresh_snapshot().await {
            return Ok(snapshot);
        }
        if let Some(result) = self.backoff_result(stale.clone()).await {
            return result;
        }

        match self.fetch_catalog().await {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                *self.cache.write().await = Some(snapshot.clone());
                *self.last_failed_refresh.write().await = None;
                Ok(snapshot)
            }
            Err(error) => {
                *self.last_failed_refresh.write().await = Some((Instant::now(), error.clone()));
                if let Some(snapshot) = stale {
                    tracing::warn!(
                        source_url = %self.source_url,
                        error = %error,
                        "Coolify catalog refresh failed; serving the last successful snapshot"
                    );
                    Ok(snapshot)
                } else {
                    Err(error)
                }
            }
        }
    }

    #[cfg(test)]
    async fn refresh_backoff_active(&self) -> bool {
        self.last_failed_refresh
            .read()
            .await
            .as_ref()
            .is_some_and(|(failed_at, _)| failed_at.elapsed() < FAILED_REFRESH_BACKOFF)
    }

    async fn backoff_result(
        &self,
        stale: Option<Arc<CatalogSnapshot>>,
    ) -> Option<Result<Arc<CatalogSnapshot>, ServiceTemplateCatalogError>> {
        let failed_refresh = self.last_failed_refresh.read().await;
        let (_, error) = failed_refresh
            .as_ref()
            .filter(|(failed_at, _)| failed_at.elapsed() < FAILED_REFRESH_BACKOFF)?;
        Some(stale.map_or_else(|| Err(error.clone()), Ok))
    }

    async fn fresh_snapshot(&self) -> Option<Arc<CatalogSnapshot>> {
        self.cache
            .read()
            .await
            .as_ref()
            .filter(|snapshot| snapshot.refreshed_at.elapsed() < CATALOG_TTL)
            .cloned()
    }

    async fn fetch_catalog(&self) -> Result<CatalogSnapshot, ServiceTemplateCatalogError> {
        let response = self
            .client
            .get(&self.source_url)
            .send()
            .await
            .map_err(|error| ServiceTemplateCatalogError::Fetch {
                url: self.source_url.clone(),
                reason: error.to_string(),
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ServiceTemplateCatalogError::HttpStatus {
                url: self.source_url.clone(),
                status: status.as_u16(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
        {
            return Err(ServiceTemplateCatalogError::CatalogTooLarge {
                url: self.source_url.clone(),
                limit_bytes: MAX_CATALOG_BYTES,
            });
        }

        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ServiceTemplateCatalogError::Fetch {
                url: self.source_url.clone(),
                reason: error.to_string(),
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
                return Err(ServiceTemplateCatalogError::CatalogTooLarge {
                    url: self.source_url.clone(),
                    limit_bytes: MAX_CATALOG_BYTES,
                });
            }
            body.extend_from_slice(&chunk);
        }

        let templates: BTreeMap<String, CoolifyTemplate> =
            serde_json::from_slice(&body).map_err(|error| {
                ServiceTemplateCatalogError::InvalidCatalog {
                    url: self.source_url.clone(),
                    reason: error.to_string(),
                }
            })?;
        if templates.len() > MAX_CATALOG_ENTRIES {
            return Err(ServiceTemplateCatalogError::TooManyEntries {
                count: templates.len(),
                limit: MAX_CATALOG_ENTRIES,
            });
        }

        let analysis_templates = templates.clone();
        let analyses = tokio::task::spawn_blocking(move || {
            analysis_templates
                .iter()
                .map(|(slug, template)| {
                    let analysis = match prepare_template(slug, template) {
                        Ok(mut prepared) => {
                            if let Err(error) =
                                temps_presets::validate_compose_credentials(&prepared.compose)
                            {
                                prepared.compatibility_issues.push(error.to_string());
                            }
                            let installable = prepared.installable();
                            let compatibility_tier = prepared.compatibility_tier();
                            CatalogTemplateAnalysis {
                                service_count: prepared.service_count,
                                backing_services: prepared.backing_services,
                                installable,
                                compatibility_tier,
                                compatibility_issues: prepared.compatibility_issues,
                                warnings: prepared.warnings,
                            }
                        }
                        Err(error) => CatalogTemplateAnalysis {
                            service_count: 0,
                            backing_services: Vec::new(),
                            installable: false,
                            compatibility_tier: TemplateCompatibilityTier::Blocked,
                            compatibility_issues: vec![error.to_string()],
                            warnings: Vec::new(),
                        },
                    };
                    (slug.clone(), analysis)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .await
        .map_err(|error| ServiceTemplateCatalogError::InvalidCatalog {
            url: self.source_url.clone(),
            reason: format!("catalog analysis task failed: {error}"),
        })?;

        Ok(CatalogSnapshot {
            templates,
            analyses,
            fetched_at: Utc::now(),
            etag,
            refreshed_at: Instant::now(),
        })
    }

    pub async fn get(
        &self,
        slug: &str,
    ) -> Result<(CoolifyTemplate, Arc<CatalogSnapshot>), ServiceTemplateCatalogError> {
        let snapshot = self.snapshot().await?;
        let template = snapshot.templates.get(slug).cloned().ok_or_else(|| {
            ServiceTemplateCatalogError::NotFound {
                slug: slug.to_string(),
            }
        })?;
        Ok((template, snapshot))
    }
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn prepare_template(
    slug: &str,
    template: &CoolifyTemplate,
) -> Result<PreparedServiceTemplate, ServiceTemplateCatalogError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&template.compose)
        .map_err(
            |error| ServiceTemplateCatalogError::InvalidComposeEncoding {
                slug: slug.to_string(),
                reason: error.to_string(),
            },
        )?;
    if bytes.len() > MAX_TEMPLATE_COMPOSE_BYTES {
        return Err(ServiceTemplateCatalogError::ComposeTooLarge {
            slug: slug.to_string(),
            limit_bytes: MAX_TEMPLATE_COMPOSE_BYTES,
        });
    }
    let compose = String::from_utf8(bytes).map_err(|error| {
        ServiceTemplateCatalogError::InvalidComposeText {
            slug: slug.to_string(),
            reason: error.to_string(),
        }
    })?;
    let mut root: YamlValue = serde_yaml::from_str(&compose).map_err(|error| {
        ServiceTemplateCatalogError::InvalidComposeYaml {
            slug: slug.to_string(),
            reason: error.to_string(),
        }
    })?;
    root.apply_merge()
        .map_err(|error| ServiceTemplateCatalogError::InvalidComposeYaml {
            slug: slug.to_string(),
            reason: format!("failed to expand YAML merge keys: {error}"),
        })?;

    let mut transformations = Vec::new();
    normalize_project_owned_names(&mut root, &mut transformations);
    externalize_literal_credentials(&mut root, &mut transformations);
    normalize_project_storage_bind_mounts(&mut root, &mut transformations);
    normalize_healthcheck_loopback_hosts(&mut root, &mut transformations);
    normalize_known_service_healthchecks(&mut root, &mut transformations);
    let mut compatibility_issues = compatibility_issues(&root);
    let requires_host_access = requires_host_access(&root);
    let mut warnings = compatibility_warnings(&root);
    normalize_published_ports(&mut root, &mut compatibility_issues, &mut transformations);
    let routes = discover_routes(&root, template, &mut compatibility_issues);
    for route in &routes {
        add_loopback_random_port(&mut root, &route.service, route.port);
    }
    declare_implicit_named_volumes(&mut root);

    let service_count = root
        .get("services")
        .and_then(YamlValue::as_mapping)
        .map(Mapping::len)
        .unwrap_or(0);
    let backing_services = discover_backing_services(&root);
    let variables = extract_variables(&root, &routes);
    for variable in variables.iter().filter(|variable| {
        variable.name.starts_with("COMPOSE_") || variable.name.starts_with("DOCKER_")
    }) {
        compatibility_issues.push(format!(
            "Variable '{}' can alter Docker Compose control-plane behavior",
            variable.name
        ));
    }
    // Capability confirmation is meaningful only for an otherwise installable
    // plan. Asking the user to approve volume initialization on a stack that
    // already requires a Docker socket or another unsupported feature implies
    // that confirmation could unblock it, which is both confusing and unsafe.
    let capability_requirements = if compatibility_issues.is_empty() {
        capability_requirements(&root)
    } else {
        Vec::new()
    };
    if !capability_requirements.is_empty() {
        warnings.push(
            "Some images commonly need limited startup permissions to initialize runtime state; confirmation is required before installation"
                .to_string(),
        );
    }
    compatibility_issues.sort();
    compatibility_issues.dedup();
    warnings.sort();
    warnings.dedup();
    let compose = serde_yaml::to_string(&root).map_err(|error| {
        ServiceTemplateCatalogError::InvalidComposeYaml {
            slug: slug.to_string(),
            reason: format!("failed to serialize normalized Compose YAML: {error}"),
        }
    })?;

    Ok(PreparedServiceTemplate {
        compose,
        service_count,
        backing_services,
        routes,
        variables,
        compatibility_issues,
        warnings,
        transformations,
        capability_requirements,
        requires_host_access,
    })
}

fn externalize_literal_credentials(
    root: &mut YamlValue,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let mut variables_by_literal = BTreeMap::<String, String>::new();
    let mut reserved_variables = extract_variables(root, &[])
        .into_iter()
        .map(|variable| variable.name)
        .collect::<BTreeSet<_>>();
    let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
        return;
    };
    for (service_name, definition) in services {
        let service_name = service_name.as_str().unwrap_or("service");
        let Some(environment) = definition
            .as_mapping_mut()
            .and_then(|service| service.get_mut("environment"))
        else {
            continue;
        };
        match environment {
            YamlValue::Sequence(entries) => {
                for entry in entries {
                    let Some(text) = entry.as_str().map(str::to_string) else {
                        continue;
                    };
                    let Some((name, value)) = text.split_once('=') else {
                        continue;
                    };
                    let name = name.to_string();
                    if should_externalize_literal_credential(&name, value) {
                        let variable = allocate_generated_credential_variable(
                            service_name,
                            &name,
                            value,
                            &mut variables_by_literal,
                            &mut reserved_variables,
                        );
                        *entry = YamlValue::String(format!("{name}=${{{variable}}}"));
                        transformations.push(TemplateTransformation {
                            code: "externalized_literal_credential",
                            description: format!(
                                "Replaced a plaintext credential default for {service_name}.{name} with a generated encrypted value"
                            ),
                        });
                    }
                }
            }
            YamlValue::Mapping(entries) => {
                for (name, value) in entries {
                    let (Some(name), Some(text)) = (name.as_str(), value.as_str()) else {
                        continue;
                    };
                    if should_externalize_literal_credential(name, text) {
                        let variable = allocate_generated_credential_variable(
                            service_name,
                            name,
                            text,
                            &mut variables_by_literal,
                            &mut reserved_variables,
                        );
                        *value = YamlValue::String(format!("${{{variable}}}"));
                        transformations.push(TemplateTransformation {
                            code: "externalized_literal_credential",
                            description: format!(
                                "Replaced a plaintext credential default for {service_name}.{name} with a generated encrypted value"
                            ),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn should_externalize_literal_credential(name: &str, value: &str) -> bool {
    temps_presets::compose_environment_name_is_secret(name)
        && !value.trim().is_empty()
        && !safe_compose_variable_reference(value)
        && !value.contains("://")
        && !temps_presets::compose_environment_value_is_safe_connection_endpoint(name, value)
        && !value.to_ascii_uppercase().contains("-----BEGIN ")
}

fn safe_compose_variable_reference(value: &str) -> bool {
    let value = value.trim();
    if let Some(name) = value.strip_prefix('$') {
        if !name.starts_with('{') {
            return is_environment_name(name);
        }
    }
    let Some(expression) = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    else {
        return false;
    };
    let name_end = expression
        .find([':', '-', '?', '+'])
        .unwrap_or(expression.len());
    let name = &expression[..name_end];
    if !is_environment_name(name) {
        return false;
    }
    let operator = &expression[name_end..];
    operator.is_empty()
        || operator.starts_with('?')
        || operator.starts_with(":?")
        || operator == ":-"
        || operator == "-"
}

fn generated_credential_variable(service_name: &str, name: &str) -> String {
    let suffix = format!("{service_name}_{name}")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("SERVICE_PASSWORD_{suffix}")
}

fn allocate_generated_credential_variable(
    service_name: &str,
    name: &str,
    literal: &str,
    variables_by_literal: &mut BTreeMap<String, String>,
    reserved_variables: &mut BTreeSet<String>,
) -> String {
    if let Some(existing) = variables_by_literal.get(literal) {
        return existing.clone();
    }

    let base = generated_credential_variable(service_name, name);
    let mut candidate = base.clone();
    if reserved_variables.contains(&candidate) {
        let digest = Sha256::digest(format!("{service_name}\0{name}").as_bytes());
        let suffix = digest[..4]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        candidate = format!("{base}_{suffix}");
        let mut discriminator = 2_u32;
        while reserved_variables.contains(&candidate) {
            candidate = format!("{base}_{suffix}_{discriminator}");
            discriminator += 1;
        }
    }

    variables_by_literal.insert(literal.to_string(), candidate.clone());
    reserved_variables.insert(candidate.clone());
    candidate
}

fn discover_backing_services(root: &YamlValue) -> Vec<TemplateBackingService> {
    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return Vec::new();
    };
    let mut backing_services = services
        .iter()
        .filter_map(|(name, definition)| {
            let service = name.as_str()?;
            let image = definition
                .as_mapping()?
                .get("image")
                .and_then(YamlValue::as_str)?;
            classify_backing_service_image(image).map(|kind| TemplateBackingService {
                service: service.to_string(),
                kind,
            })
        })
        .collect::<Vec<_>>();
    backing_services.sort();
    backing_services.dedup();
    backing_services
}

fn classify_backing_service_image(image: &str) -> Option<TemplateBackingServiceKind> {
    let repository = normalized_image_repository(image);
    let image_name = repository.rsplit('/').next().unwrap_or(repository.as_str());

    if image_name == "postgres"
        || matches!(repository.as_str(), "postgis/postgis" | "pgvector/pgvector")
        || repository.starts_with("timescale/timescaledb")
        || repository.starts_with("supabase/postgres")
    {
        return Some(TemplateBackingServiceKind::Postgres);
    }
    if image_name == "redis"
        || matches!(image_name, "redis-stack" | "redis-stack-server")
        || image_name == "valkey"
        || image_name == "dragonfly"
    {
        return Some(TemplateBackingServiceKind::Redis);
    }
    if image_name == "mongo" || image_name == "mongodb" {
        return Some(TemplateBackingServiceKind::MongoDb);
    }
    if image_name == "minio" || image_name == "rustfs" || image_name == "garage" {
        return Some(TemplateBackingServiceKind::S3);
    }
    None
}

fn normalized_image_repository(image: &str) -> String {
    let image = image.trim().split('@').next().unwrap_or_default();
    let last_slash = image.rfind('/');
    match image.rfind(':') {
        Some(colon) if last_slash.is_none_or(|slash| colon > slash) => &image[..colon],
        _ => image,
    }
    .to_ascii_lowercase()
}

/// Prefer an explicit IPv4 loopback address in HTTP container healthchecks.
/// Several minimal images resolve `localhost` to `::1` first while their app
/// listens only on IPv4. The service is then fully operational but Docker marks
/// it unhealthy forever. Limit rewriting to healthcheck test values and require
/// a URL authority boundary so lookalikes such as `localhost.example.com` are
/// never changed.
fn normalize_healthcheck_loopback_hosts(
    root: &mut YamlValue,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
        return;
    };
    for (name, definition) in services {
        let service_name = name.as_str().unwrap_or("service");
        let Some(test) = definition
            .as_mapping_mut()
            .and_then(|service| service.get_mut("healthcheck"))
            .and_then(YamlValue::as_mapping_mut)
            .and_then(|healthcheck| healthcheck.get_mut("test"))
        else {
            continue;
        };
        if rewrite_healthcheck_http_localhost(test) {
            transformations.push(TemplateTransformation {
                code: "normalize_healthcheck_loopback",
                description: format!(
                    "Changed service '{service_name}' HTTP health probe from localhost to the IPv4 loopback address"
                ),
            });
        }
    }
}

fn rewrite_healthcheck_http_localhost(value: &mut YamlValue) -> bool {
    match value {
        YamlValue::String(text) => {
            let normalized = replace_http_localhost(text);
            if normalized == *text {
                false
            } else {
                *text = normalized;
                true
            }
        }
        YamlValue::Sequence(values) => {
            let mut changed = false;
            for value in values {
                changed |= rewrite_healthcheck_http_localhost(value);
            }
            changed
        }
        YamlValue::Mapping(values) => {
            let mut changed = false;
            for value in values.values_mut() {
                changed |= rewrite_healthcheck_http_localhost(value);
            }
            changed
        }
        YamlValue::Tagged(value) => rewrite_healthcheck_http_localhost(&mut value.value),
        _ => false,
    }
}

fn replace_http_localhost(input: &str) -> String {
    const NEEDLE: &str = "http://localhost";
    const REPLACEMENT: &str = "http://127.0.0.1";

    let mut remainder = input;
    let mut output = String::with_capacity(input.len());
    while let Some(index) = remainder.to_ascii_lowercase().find(NEEDLE) {
        output.push_str(&remainder[..index]);
        let after = &remainder[index + NEEDLE.len()..];
        if after
            .chars()
            .next()
            .is_none_or(|character| matches!(character, ':' | '/' | '?' | '#'))
        {
            output.push_str(REPLACEMENT);
        } else {
            output.push_str(&remainder[index..index + NEEDLE.len()]);
        }
        remainder = after;
    }
    output.push_str(remainder);
    output
}

/// Correct known upstream probes that only prove a bundled web server is up.
/// Activepieces serves its static UI through nginx on port 80 while its API
/// listens behind nginx on port 3000. Probing `/api/v1/health` therefore waits
/// for the application itself instead of declaring a half-started container
/// ready as soon as nginx can return `index.html`.
fn normalize_known_service_healthchecks(
    root: &mut YamlValue,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
        return;
    };
    for (name, definition) in services {
        let service_name = name.as_str().unwrap_or("service");
        let Some(service) = definition.as_mapping_mut() else {
            continue;
        };
        let is_activepieces = service
            .get("image")
            .and_then(YamlValue::as_str)
            .is_some_and(|image| is_activepieces_image(&normalized_image_repository(image)));
        if !is_activepieces {
            continue;
        }
        let Some(healthcheck) = service.get_mut("healthcheck") else {
            continue;
        };
        if temps_presets::http_healthcheck_path(healthcheck).as_deref() != Some("/") {
            continue;
        }
        let ports = healthcheck_ports(healthcheck);
        let Some(port) = (ports.len() == 1)
            .then(|| ports.iter().next().copied())
            .flatten()
        else {
            continue;
        };
        let Some(healthcheck) = healthcheck.as_mapping_mut() else {
            continue;
        };
        healthcheck.insert(
            YamlValue::String("test".to_string()),
            YamlValue::Sequence(vec![
                YamlValue::String("CMD".to_string()),
                YamlValue::String("curl".to_string()),
                YamlValue::String("-f".to_string()),
                YamlValue::String(format!("http://127.0.0.1:{port}/api/v1/health")),
            ]),
        );
        transformations.push(TemplateTransformation {
            code: "normalize_healthcheck",
            description: format!(
                "Changed {service_name}'s health probe from the static root page to Activepieces' application health endpoint"
            ),
        });
    }
}

fn is_activepieces_image(repository: &str) -> bool {
    matches!(
        repository,
        "activepieces/activepieces" | "ghcr.io/activepieces/activepieces"
    )
}

fn first_service_name(root: &YamlValue) -> Option<String> {
    root.get("services")
        .and_then(YamlValue::as_mapping)
        .and_then(|services| services.keys().filter_map(YamlValue::as_str).next())
        .map(str::to_string)
}

#[derive(Debug)]
struct RouteCandidate {
    service: String,
    variable_names: Vec<String>,
    declared_ports: BTreeSet<u16>,
}

fn discover_routes(
    root: &YamlValue,
    template: &CoolifyTemplate,
    issues: &mut Vec<String>,
) -> Vec<TemplateRoute> {
    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for (name, definition) in services {
        let (Some(name), Some(service)) = (name.as_str(), definition.as_mapping()) else {
            continue;
        };
        let entries = service
            .get("environment")
            .map(environment_entries)
            .unwrap_or_default();
        let mut variable_names = Vec::new();
        let mut declared_ports = BTreeSet::new();
        for (key, _) in entries {
            if !is_public_magic_variable(&key) {
                continue;
            }
            if let Some(port) = magic_variable_port(&key) {
                declared_ports.insert(port);
            }
            variable_names.push(key);
        }
        if !variable_names.is_empty() {
            variable_names.sort();
            variable_names.dedup();
            candidates.push(RouteCandidate {
                service: name.to_string(),
                variable_names,
                declared_ports,
            });
        }
    }

    let template_port = match template.port.as_deref() {
        Some(value) => match value.parse::<u16>() {
            Ok(port) if port > 0 => Some(port),
            _ => {
                issues.push(format!("Template port '{value}' is not a single TCP port"));
                None
            }
        },
        None => None,
    };
    if candidates.is_empty() {
        if let (Some(service), Some(port)) = (first_service_name(root), template_port) {
            return vec![TemplateRoute {
                health_check_path: infer_service_health_path(root, &service),
                service,
                port,
                variable_names: Vec::new(),
            }];
        }
        return Vec::new();
    }

    let only_candidate = candidates.len() == 1;
    let mut routes = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.into_iter().enumerate() {
        let port = if candidate.declared_ports.len() == 1 {
            candidate.declared_ports.iter().next().copied()
        } else if candidate.declared_ports.len() > 1 {
            issues.push(format!(
                "Service '{}' declares public URL variables for multiple ports; Temps supports one public URL per Compose service",
                candidate.service
            ));
            None
        } else if only_candidate || index == 0 {
            template_port.or_else(|| infer_service_port(root, &candidate.service))
        } else {
            infer_service_port(root, &candidate.service)
        };
        match port {
            Some(port) => routes.push(TemplateRoute {
                health_check_path: infer_service_health_path(root, &candidate.service),
                service: candidate.service,
                port,
                variable_names: candidate.variable_names,
            }),
            None => issues.push(format!(
                "Service '{}' declares a public URL but does not identify a single routable port",
                candidate.service
            )),
        }
    }
    routes
}

fn infer_service_health_path(root: &YamlValue, service_name: &str) -> Option<String> {
    root.get("services")?
        .as_mapping()?
        .get(service_name)?
        .as_mapping()?
        .get("healthcheck")
        .and_then(temps_presets::http_healthcheck_path)
}

fn is_public_magic_variable(key: &str) -> bool {
    key.starts_with("SERVICE_URL_") || key.starts_with("SERVICE_FQDN_")
}

fn magic_variable_port(key: &str) -> Option<u16> {
    is_public_magic_variable(key)
        .then(|| key.rsplit('_').next())
        .flatten()
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0)
}

fn infer_service_port(root: &YamlValue, service_name: &str) -> Option<u16> {
    let service = root
        .get("services")?
        .as_mapping()?
        .get(service_name)?
        .as_mapping()?;
    for ports in [
        service
            .get("ports")
            .and_then(YamlValue::as_sequence)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(compose_target_port)
                    .collect::<BTreeSet<_>>()
            }),
        service
            .get("expose")
            .and_then(YamlValue::as_sequence)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(compose_exposed_port)
                    .collect::<BTreeSet<_>>()
            }),
        service
            .get("healthcheck")
            .map(healthcheck_ports)
            .filter(|ports| !ports.is_empty()),
    ]
    .into_iter()
    .flatten()
    {
        if ports.len() == 1 {
            return ports.into_iter().next();
        }
    }
    None
}

fn compose_exposed_port(value: &YamlValue) -> Option<u16> {
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .or_else(|| {
            value.as_str().and_then(|value| {
                value
                    .split('/')
                    .next()
                    .and_then(|port| port.parse::<u16>().ok())
            })
        })
        .filter(|port| *port > 0)
}

fn healthcheck_ports(value: &YamlValue) -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    visit_yaml_strings(value, &mut |text| {
        for marker in ["localhost:", "127.0.0.1:", "0.0.0.0:"] {
            let mut remainder = text;
            while let Some(index) = remainder.find(marker) {
                let after = &remainder[index + marker.len()..];
                let digits = after
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>();
                if let Ok(port) = digits.parse::<u16>() {
                    if port > 0 {
                        ports.insert(port);
                    }
                }
                remainder = after;
            }
        }
    });
    ports
}

fn normalize_project_owned_names(
    root: &mut YamlValue,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let Some(root_mapping) = root.as_mapping_mut() else {
        return;
    };
    if root_mapping.remove("name").is_some() {
        transformations.push(TemplateTransformation {
            code: "remove_project_name",
            description: "Removed the fixed Compose project name so Temps can isolate the stack"
                .to_string(),
        });
    }
    let Some(services) = root_mapping
        .get_mut("services")
        .and_then(YamlValue::as_mapping_mut)
    else {
        return;
    };
    for (name, definition) in services {
        let Some(service) = definition.as_mapping_mut() else {
            continue;
        };
        if service.remove("container_name").is_some() {
            transformations.push(TemplateTransformation {
                code: "remove_container_name",
                description: format!(
                    "Removed service '{}' fixed container name to prevent cross-project collisions",
                    name.as_str().unwrap_or("<non-string>")
                ),
            });
        }
    }
}

fn normalize_published_ports(
    root: &mut YamlValue,
    issues: &mut Vec<String>,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
        return;
    };
    for (name, definition) in services {
        let name = name.as_str().unwrap_or("<non-string>");
        let Some(service) = definition.as_mapping_mut() else {
            continue;
        };
        let Some(ports_value) = service.get_mut("ports") else {
            continue;
        };
        let Some(entries) = ports_value.as_sequence() else {
            issues.push(format!("Service '{name}' ports must be a Compose sequence"));
            continue;
        };
        let mut normalized = BTreeSet::new();
        let mut invalid = false;
        for entry in entries {
            let Some((target, protocol)) = compose_port_target_and_protocol(entry) else {
                invalid = true;
                break;
            };
            let protocol_suffix = if protocol == "tcp" {
                String::new()
            } else {
                format!("/{protocol}")
            };
            normalized.insert(format!("127.0.0.1::{target}{protocol_suffix}"));
        }
        if invalid {
            issues.push(format!(
                "Service '{name}' has a port mapping that cannot be safely normalized"
            ));
            continue;
        }
        *ports_value = YamlValue::Sequence(normalized.into_iter().map(YamlValue::String).collect());
        transformations.push(TemplateTransformation {
            code: "normalize_ports",
            description: format!(
                "Replaced service '{name}' fixed host ports with random loopback-only bindings"
            ),
        });
    }
}

fn compose_port_target_and_protocol(value: &YamlValue) -> Option<(u16, String)> {
    if let Some(value) = value.as_str() {
        let (mapping, protocol) = value
            .rsplit_once('/')
            .filter(|(_, protocol)| matches!(*protocol, "tcp" | "udp"))
            .map_or((value, "tcp"), |(mapping, protocol)| (mapping, protocol));
        if mapping.contains('-') || mapping.contains('$') {
            return None;
        }
        let target = mapping.rsplit(':').next()?.parse::<u16>().ok()?;
        return (target > 0).then(|| (target, protocol.to_string()));
    }
    let mapping = value.as_mapping()?;
    let target = mapping
        .get("target")
        .and_then(YamlValue::as_u64)
        .and_then(|target| u16::try_from(target).ok())?;
    let protocol = mapping
        .get("protocol")
        .and_then(YamlValue::as_str)
        .unwrap_or("tcp");
    (target > 0 && matches!(protocol, "tcp" | "udp")).then(|| (target, protocol.to_string()))
}

fn add_loopback_random_port(root: &mut YamlValue, service_name: &str, port: u16) {
    let Some(service) = root
        .get_mut("services")
        .and_then(YamlValue::as_mapping_mut)
        .and_then(|services| services.get_mut(service_name))
        .and_then(YamlValue::as_mapping_mut)
    else {
        return;
    };
    let ports = service
        .entry(YamlValue::String("ports".to_string()))
        .or_insert_with(|| YamlValue::Sequence(Vec::new()));
    let Some(ports) = ports.as_sequence_mut() else {
        return;
    };
    let already_declared = ports
        .iter()
        .any(|entry| compose_target_port(entry) == Some(port));
    if !already_declared {
        ports.push(YamlValue::String(format!("127.0.0.1::{port}")));
    }
}

fn compose_target_port(value: &YamlValue) -> Option<u16> {
    compose_port_target_and_protocol(value)
        .filter(|(_, protocol)| protocol == "tcp")
        .map(|(target, _)| target)
}

/// Convert a narrowly-defined app-owned host path into a project-scoped named
/// volume. Some upstream templates spell writable runtime state as
/// `/service/config:/config`; mounting that host path is neither portable nor
/// allowed by Temps, while a named volume preserves the intended persistence
/// without granting access outside the project.
fn normalize_project_storage_bind_mounts(
    root: &mut YamlValue,
    transformations: &mut Vec<TemplateTransformation>,
) {
    let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
        return;
    };
    for (service_name, definition) in services {
        let Some(service_name) = service_name.as_str() else {
            continue;
        };
        let Some(volumes) = definition
            .as_mapping_mut()
            .and_then(|service| service.get_mut("volumes"))
            .and_then(YamlValue::as_sequence_mut)
        else {
            continue;
        };
        for volume in volumes {
            let Some((source, target, mode)) = short_bind_mount_parts(volume) else {
                continue;
            };
            if !is_project_storage_bind_mount(service_name, &source, &target, mode.as_deref()) {
                continue;
            }
            let volume_name = project_storage_volume_name(service_name, &target);
            let replacement = mode.as_deref().map_or_else(
                || format!("{volume_name}:{target}"),
                |mode| format!("{volume_name}:{target}:{mode}"),
            );
            *volume = YamlValue::String(replacement);
            transformations.push(TemplateTransformation {
                code: "project_storage_volume",
                description: format!(
                    "Converted app-owned path {source} for {service_name} into project volume {volume_name}"
                ),
            });
        }
    }
}

fn short_bind_mount_parts(volume: &YamlValue) -> Option<(String, String, Option<String>)> {
    let volume = volume.as_str()?;
    let fields = volume.split(':').map(str::trim).collect::<Vec<_>>();
    if !(2..=3).contains(&fields.len()) || !fields[0].starts_with('/') {
        return None;
    }
    Some((
        fields[0].to_string(),
        fields[1].to_string(),
        fields.get(2).map(|mode| (*mode).to_string()),
    ))
}

fn is_project_storage_bind_mount(
    service_name: &str,
    source: &str,
    target: &str,
    mode: Option<&str>,
) -> bool {
    if mode.is_some_and(|mode| {
        mode.split(',')
            .any(|option| !matches!(option, "rw" | "z" | "Z"))
    }) || !target.starts_with('/')
    {
        return false;
    }
    let source_parts = source
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(target_leaf) = target.trim_end_matches('/').rsplit('/').next() else {
        return false;
    };
    let Some(source_owner) = source_parts
        .first()
        .map(|owner| normalized_storage_component(owner))
    else {
        return false;
    };
    source_parts.len() == 2
        && !is_system_root_component(&source_owner)
        && source_owner == normalized_storage_component(service_name)
        && source_parts[1].eq_ignore_ascii_case(target_leaf)
        && matches!(
            target_leaf.to_ascii_lowercase().as_str(),
            "config" | "data" | "storage" | "uploads" | "files" | "cache"
        )
}

fn is_system_root_component(component: &str) -> bool {
    matches!(
        component,
        // Linux/FHS roots plus common host-runtime mount roots.
        "bin"
            | "boot"
            | "dev"
            | "docker"
            | "etc"
            | "home"
            | "host-mnt"
            | "lib"
            | "lib32"
            | "lib64"
            | "libx32"
            | "lost-found"
            | "media"
            | "mnt"
            | "nix"
            | "opt"
            | "proc"
            | "root"
            | "run"
            | "sbin"
            | "snap"
            | "srv"
            | "sys"
            | "tmp"
            | "usr"
            | "var"
            // macOS host roots visible to Docker Desktop bind mounts.
            | "applications"
            | "library"
            | "network"
            | "private"
            | "system"
            | "users"
            | "volumes"
    )
}

fn project_storage_volume_name(service_name: &str, target: &str) -> String {
    let target_leaf = target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("storage");
    format!(
        "{}-{}",
        normalized_storage_component(service_name),
        normalized_storage_component(target_leaf)
    )
}

fn normalized_storage_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn declare_implicit_named_volumes(root: &mut YamlValue) {
    let mut names = BTreeSet::new();
    if let Some(services) = root.get("services").and_then(YamlValue::as_mapping) {
        for definition in services.values() {
            let Some(volumes) = definition
                .as_mapping()
                .and_then(|service| service.get("volumes"))
                .and_then(YamlValue::as_sequence)
            else {
                continue;
            };
            for volume in volumes {
                let source = if let Some(short) = volume.as_str() {
                    short.split(':').next().map(str::trim)
                } else {
                    volume
                        .as_mapping()
                        .and_then(|mapping| mapping.get("source"))
                        .and_then(YamlValue::as_str)
                };
                if let Some(source) = source.filter(|source| is_named_volume(source)) {
                    names.insert(source.to_string());
                }
            }
        }
    }
    if names.is_empty() {
        return;
    }
    let Some(root) = root.as_mapping_mut() else {
        return;
    };
    let volumes = root
        .entry(YamlValue::String("volumes".to_string()))
        .or_insert_with(|| YamlValue::Mapping(Mapping::new()));
    let Some(volumes) = volumes.as_mapping_mut() else {
        return;
    };
    for name in names {
        volumes
            .entry(YamlValue::String(name))
            .or_insert_with(|| YamlValue::Mapping(Mapping::new()));
    }
}

fn is_named_volume(source: &str) -> bool {
    !source.is_empty()
        && !source.starts_with('.')
        && !source.starts_with('/')
        && !source.starts_with('~')
        && !source.starts_with('$')
        && !source.contains('/')
}

fn compatibility_issues(root: &YamlValue) -> Vec<String> {
    let mut issues = Vec::new();
    let Some(root_mapping) = root.as_mapping() else {
        return vec!["Compose document must be a mapping".to_string()];
    };
    if root_mapping.contains_key("include") {
        issues.push("Top-level include can load files outside the reviewed template".to_string());
    }
    for section in ["volumes", "networks", "configs", "secrets"] {
        if let Some(items) = root.get(section).and_then(YamlValue::as_mapping) {
            for (name, definition) in items {
                let Some(definition) = definition.as_mapping() else {
                    continue;
                };
                if definition.get("external").and_then(YamlValue::as_bool) == Some(true)
                    || definition.contains_key("name")
                {
                    issues.push(format!(
                        "Top-level {section} entry '{}' can attach to a shared Docker resource",
                        name.as_str().unwrap_or("<non-string>")
                    ));
                }
                if matches!(section, "volumes" | "networks") && !definition.is_empty() {
                    issues.push(format!(
                        "Top-level {section} entry '{}' customizes a Docker resource; only project-scoped defaults are supported",
                        name.as_str().unwrap_or("<non-string>")
                    ));
                }
            }
        }
    }
    for section in ["configs", "secrets"] {
        if root
            .get(section)
            .and_then(YamlValue::as_mapping)
            .is_some_and(|items| {
                items.values().any(|item| {
                    item.as_mapping()
                        .is_some_and(|mapping| mapping.contains_key("file"))
                })
            })
        {
            issues.push(format!(
                "Top-level {section} references files not included in the catalog entry"
            ));
        }
    }
    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        issues.push("Compose document has no services mapping".to_string());
        return issues;
    };
    for (name, definition) in services {
        let name = name.as_str().unwrap_or("<non-string>");
        let Some(service) = definition.as_mapping() else {
            continue;
        };
        for field in [
            "build",
            "cap_add",
            "cgroup",
            "cgroup_parent",
            "credential_spec",
            "device_cgroup_rules",
            "devices",
            "extends",
            "external_links",
            "gpus",
            "group_add",
            "isolation",
            "label_file",
            "network_mode",
            "oom_kill_disable",
            "pid",
            "ipc",
            "runtime",
            "security_opt",
            "storage_opt",
            "sysctls",
            "tmpfs",
            "ulimits",
            "use_api_socket",
            "userns_mode",
            "uts",
            "volumes_from",
            "post_start",
            "pre_stop",
            "provider",
        ] {
            if service.contains_key(field) {
                issues.push(format!(
                    "Service '{name}' uses unsupported host-affecting field '{field}'"
                ));
            }
        }
        if !service.contains_key("image") && !service.contains_key("build") {
            issues.push(format!(
                "Service '{name}' has neither an image nor a supported build definition"
            ));
        }
        if service.get("privileged").and_then(YamlValue::as_bool) == Some(true) {
            issues.push(format!("Service '{name}' requests privileged mode"));
        }
        for field in ["privileged", "shm_size", "cgroup"] {
            if service.get(field).is_some_and(value_contains_interpolation) {
                issues.push(format!(
                    "Service '{name}' interpolates security-guarded field '{field}'"
                ));
            }
        }
        if service
            .get("deploy")
            .and_then(YamlValue::as_mapping)
            .and_then(|deploy| deploy.get("resources"))
            .and_then(YamlValue::as_mapping)
            .and_then(|resources| resources.get("reservations"))
            .and_then(YamlValue::as_mapping)
            .is_some_and(|reservations| reservations.contains_key("devices"))
        {
            issues.push(format!(
                "Service '{name}' requests host devices through deploy resource reservations"
            ));
        }
        if let Some(env_file) = service.get("env_file") {
            for path in env_file_paths(env_file) {
                if is_unsafe_volume_source(path) {
                    issues.push(format!(
                        "Service '{name}' references unsafe env file path '{path}'"
                    ));
                }
            }
        }
        if let Some(volumes) = service.get("volumes").and_then(YamlValue::as_sequence) {
            for volume in volumes {
                if value_contains_interpolation(volume) {
                    issues.push(format!(
                        "Service '{name}' interpolates security-guarded field 'volumes'"
                    ));
                }
                let source = volume_source(volume);
                if source.is_some_and(is_unsafe_volume_source) {
                    issues.push(format!(
                        "Service '{name}' mounts host path '{}'",
                        source.unwrap_or_default()
                    ));
                }
            }
        }
    }
    issues
}

/// Return true when installing the document would require authority outside
/// the project-scoped Compose sandbox. This is intentionally separate from
/// generic incompatibility: a missing bundled file may become supportable,
/// while a Docker socket or host namespace is an administrator-level trust
/// decision and must never be presented as ordinary "manual work".
fn requires_host_access(root: &YamlValue) -> bool {
    if root
        .get("volumes")
        .and_then(YamlValue::as_mapping)
        .is_some_and(|items| {
            items.values().any(|definition| {
                definition.as_mapping().is_some_and(|definition| {
                    definition.get("external").and_then(YamlValue::as_bool) == Some(true)
                        || definition.contains_key("name")
                        || !definition.is_empty()
                })
            })
        })
        || root
            .get("networks")
            .and_then(YamlValue::as_mapping)
            .is_some_and(|items| {
                items.values().any(|definition| {
                    definition.as_mapping().is_some_and(|definition| {
                        definition.get("external").and_then(YamlValue::as_bool) == Some(true)
                            || definition.contains_key("name")
                            || !definition.is_empty()
                    })
                })
            })
    {
        return true;
    }

    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return false;
    };
    services.values().any(|definition| {
        let Some(service) = definition.as_mapping() else {
            return false;
        };
        if service.get("privileged").and_then(YamlValue::as_bool) == Some(true)
            || [
                "cap_add",
                "cgroup",
                "cgroup_parent",
                "credential_spec",
                "device_cgroup_rules",
                "devices",
                "gpus",
                "group_add",
                "isolation",
                "network_mode",
                "pid",
                "ipc",
                "runtime",
                "security_opt",
                "storage_opt",
                "sysctls",
                "use_api_socket",
                "userns_mode",
                "uts",
                "volumes_from",
            ]
            .iter()
            .any(|field| service.contains_key(*field))
            || service
                .get("deploy")
                .and_then(YamlValue::as_mapping)
                .and_then(|deploy| deploy.get("resources"))
                .and_then(YamlValue::as_mapping)
                .and_then(|resources| resources.get("reservations"))
                .and_then(YamlValue::as_mapping)
                .is_some_and(|reservations| reservations.contains_key("devices"))
        {
            return true;
        }
        service
            .get("volumes")
            .and_then(YamlValue::as_sequence)
            .is_some_and(|volumes| {
                volumes.iter().any(|volume| {
                    value_contains_interpolation(volume)
                        || volume_source(volume).is_some_and(is_unsafe_volume_source)
                })
            })
    })
}

fn compatibility_warnings(root: &YamlValue) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return warnings;
    };
    for (name, definition) in services {
        let name = name.as_str().unwrap_or("<non-string>");
        let Some(service) = definition.as_mapping() else {
            continue;
        };
        if service.contains_key("env_file") {
            warnings.push(format!(
                "Service '{name}' references env_file content; missing relative files will be generated from the project environment"
            ));
        }
        if service.contains_key("deploy") || service.contains_key("scale") {
            warnings.push(format!(
                "Service '{name}' includes Compose scaling/resource settings that should be reviewed for this server"
            ));
        }
    }
    warnings
}

fn value_contains_interpolation(value: &YamlValue) -> bool {
    match value {
        YamlValue::String(value) => value.contains('$'),
        YamlValue::Sequence(values) => values.iter().any(value_contains_interpolation),
        YamlValue::Mapping(values) => values.values().any(value_contains_interpolation),
        YamlValue::Tagged(value) => value_contains_interpolation(&value.value),
        _ => false,
    }
}

fn env_file_paths(value: &YamlValue) -> Vec<&str> {
    match value {
        YamlValue::String(path) => vec![path.as_str()],
        YamlValue::Sequence(entries) => entries
            .iter()
            .filter_map(|entry| {
                entry.as_str().or_else(|| {
                    entry
                        .as_mapping()
                        .and_then(|mapping| mapping.get("path"))
                        .and_then(YamlValue::as_str)
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn capability_requirements(root: &YamlValue) -> Vec<TemplateCapabilityRequirement> {
    let Ok(compose) = serde_yaml::to_string(root) else {
        return Vec::new();
    };
    let Ok(services) = temps_presets::list_compose_services(&compose) else {
        return Vec::new();
    };
    let database_services = services
        .into_iter()
        .filter(|service| service.looks_like_database)
        .map(|service| service.name)
        .collect::<BTreeSet<_>>();

    let Some(compose_services) = root.get("services").and_then(YamlValue::as_mapping) else {
        return Vec::new();
    };
    compose_services
        .iter()
        .filter_map(|(name, definition)| {
            let name = name.as_str()?;
            let database = database_services.contains(name);
            let persistent_volume = service_has_writable_volume(definition);
            let known_runtime_reason = known_runtime_capability_reason(definition);
            (database || persistent_volume || known_runtime_reason.is_some()).then(|| TemplateCapabilityRequirement {
                service: name.to_string(),
                capability: "relaxed_linux_capabilities",
                reason: if database {
                    "This image commonly initializes persistent data as root before dropping to its runtime user"
                        .to_string()
                } else if let Some(reason) = known_runtime_reason {
                    reason.to_string()
                } else {
                    "This service has a writable Docker volume; some images need limited ownership capabilities to initialize the empty volume"
                        .to_string()
                },
            })
        })
        .collect()
}

fn known_runtime_capability_reason(definition: &YamlValue) -> Option<&'static str> {
    let repository = definition
        .as_mapping()?
        .get("image")
        .and_then(YamlValue::as_str)
        .map(normalized_image_repository)?;
    is_activepieces_image(&repository).then_some(
        "This Activepieces image starts nginx as root and needs limited ownership and user-switch capabilities before it can serve port 80",
    )
}

fn service_has_writable_volume(definition: &YamlValue) -> bool {
    definition
        .get("volumes")
        .and_then(YamlValue::as_sequence)
        .is_some_and(|volumes| volumes.iter().any(is_writable_volume))
}

fn is_writable_volume(volume: &YamlValue) -> bool {
    if let Some(short) = volume.as_str() {
        let fields = short.split(':').map(str::trim).collect::<Vec<_>>();
        let read_only = fields
            .last()
            .is_some_and(|mode| mode.split(',').any(|option| option == "ro"));
        if read_only {
            return false;
        }
        return fields.len() == 1 || fields.first().is_some_and(|source| is_named_volume(source));
    }

    let Some(mapping) = volume.as_mapping() else {
        return false;
    };
    if mapping
        .get("read_only")
        .and_then(YamlValue::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    !matches!(
        mapping.get("type").and_then(YamlValue::as_str),
        Some("bind" | "tmpfs")
    )
}

fn volume_source(value: &YamlValue) -> Option<&str> {
    if let Some(short) = value.as_str() {
        return short.split(':').next().map(str::trim);
    }
    value
        .as_mapping()
        .and_then(|mapping| mapping.get("source"))
        .and_then(YamlValue::as_str)
}

fn is_unsafe_volume_source(source: &str) -> bool {
    source.starts_with('/')
        || source.starts_with('~')
        || source.starts_with("../")
        || source.contains("/../")
        || source.contains('$')
}

fn environment_entries(value: &YamlValue) -> Vec<(String, Option<String>)> {
    match value {
        YamlValue::Sequence(entries) => entries
            .iter()
            .filter_map(YamlValue::as_str)
            .filter_map(|entry| {
                let (key, value) = entry
                    .split_once('=')
                    .map_or((entry, None), |(key, value)| (key, Some(value)));
                let key = key.trim();
                is_environment_name(key).then(|| (key.to_string(), value.map(str::to_string)))
            })
            .collect(),
        YamlValue::Mapping(entries) => entries
            .iter()
            .filter_map(|(key, value)| {
                let key = key.as_str().filter(|key| is_environment_name(key))?;
                Some((key.to_string(), value.as_str().map(str::to_string)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_variables(root: &YamlValue, routes: &[TemplateRoute]) -> Vec<TemplateVariable> {
    let mut found: BTreeMap<String, TemplateVariable> = BTreeMap::new();
    visit_yaml_strings(root, &mut |value| {
        extract_interpolated_variables(value, &mut found);
    });
    if let Some(services) = root.get("services").and_then(YamlValue::as_mapping) {
        for definition in services.values() {
            let Some(environment) = definition
                .as_mapping()
                .and_then(|service| service.get("environment"))
            else {
                continue;
            };
            match environment {
                YamlValue::Sequence(entries) => {
                    for entry in entries.iter().filter_map(YamlValue::as_str) {
                        let (key, assigned) = entry
                            .split_once('=')
                            .map_or((entry, None), |(key, value)| (key, Some(value)));
                        let key = key.trim();
                        if is_environment_name(key)
                            && (assigned.is_none()
                                || key.starts_with("SERVICE_URL_")
                                || key.starts_with("SERVICE_FQDN_"))
                        {
                            insert_variable(
                                &mut found,
                                key,
                                assigned
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string),
                                false,
                            );
                        }
                    }
                }
                YamlValue::Mapping(entries) => {
                    for (key, value) in entries {
                        let Some(key) = key.as_str().filter(|key| is_environment_name(key)) else {
                            continue;
                        };
                        if value.is_null() {
                            insert_variable(&mut found, key, None, false);
                        } else if key.starts_with("SERVICE_URL_")
                            || key.starts_with("SERVICE_FQDN_")
                        {
                            insert_variable(
                                &mut found,
                                key,
                                value.as_str().map(str::to_string),
                                true,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for route in routes {
        for declared_name in &route.variable_names {
            for name in related_public_variable_names(declared_name, route.port) {
                let default_value = (name == *declared_name)
                    .then(|| {
                        found
                            .get(declared_name)
                            .and_then(|value| value.default_value.clone())
                    })
                    .flatten();
                found
                    .entry(name.clone())
                    .and_modify(|variable| {
                        variable.route_service = Some(route.service.clone());
                        variable.required |= name == *declared_name;
                    })
                    .or_insert_with(|| TemplateVariable {
                        kind: variable_kind(&name),
                        required: name == *declared_name,
                        name,
                        default_value,
                        route_service: Some(route.service.clone()),
                    });
            }
        }
    }
    found.into_values().collect()
}

fn related_public_variable_names(name: &str, route_port: u16) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if !is_public_magic_variable(name) {
        return names;
    }
    let suffix = name
        .strip_prefix("SERVICE_URL_")
        .or_else(|| name.strip_prefix("SERVICE_FQDN_"))
        .unwrap_or_default();
    let port_suffix = format!("_{route_port}");
    let base_suffix = suffix.strip_suffix(&port_suffix).unwrap_or(suffix);
    for prefix in ["SERVICE_URL_", "SERVICE_FQDN_"] {
        names.insert(format!("{prefix}{base_suffix}"));
        names.insert(format!("{prefix}{base_suffix}_{route_port}"));
    }
    names
}

fn visit_yaml_strings(value: &YamlValue, visitor: &mut impl FnMut(&str)) {
    match value {
        YamlValue::String(value) => visitor(value),
        YamlValue::Sequence(values) => {
            for value in values {
                visit_yaml_strings(value, visitor);
            }
        }
        YamlValue::Mapping(values) => {
            for (key, value) in values {
                visit_yaml_strings(key, visitor);
                visit_yaml_strings(value, visitor);
            }
        }
        YamlValue::Tagged(value) => visit_yaml_strings(&value.value, visitor),
        _ => {}
    }
}

fn extract_interpolated_variables(value: &str, found: &mut BTreeMap<String, TemplateVariable>) {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' || index + 1 >= bytes.len() || bytes[index + 1] == b'$' {
            index += 1;
            continue;
        }
        if bytes[index + 1] == b'{' {
            let Some(close) = value[index + 2..].find('}') else {
                break;
            };
            let expression = &value[index + 2..index + 2 + close];
            let name_end = expression
                .find([':', '-', '?', '+'])
                .unwrap_or(expression.len());
            let name = &expression[..name_end];
            if is_environment_name(name) {
                let operator = &expression[name_end..];
                let default_expression = operator
                    .strip_prefix(":-")
                    .or_else(|| operator.strip_prefix('-'));
                // Leave nested defaults to Compose. Sending a literal value
                // like "$SERVICE_FQDN_APP" would suppress Compose's second
                // interpolation and break the generated dependency.
                let default = default_expression
                    .filter(|value| !value.contains('$'))
                    .map(str::to_string);
                // Docker Compose substitutes an empty value for `${VAR}` and
                // `$VAR`. Only the `?` / `:?` operators make interpolation
                // fail when the value is absent. Optional app integrations
                // (SMTP, AI providers, OAuth, etc.) commonly use plain
                // interpolation and must not block installation.
                let required = operator.starts_with(":?") || operator.starts_with('?');
                insert_variable(found, name, default, required);
            }
            // Advance one byte so nested variables inside a default expression
            // are discovered by the same scanner.
            index += 1;
            continue;
        }
        let start = index + 1;
        let end = bytes[start..]
            .iter()
            .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .map(|offset| start + offset)
            .unwrap_or(bytes.len());
        let name = &value[start..end];
        if is_environment_name(name) {
            insert_variable(found, name, None, false);
        }
        index = end.max(index + 1);
    }
}

fn insert_variable(
    found: &mut BTreeMap<String, TemplateVariable>,
    name: &str,
    default_value: Option<String>,
    required: bool,
) {
    let kind = variable_kind(name);
    let default_value = default_value.or_else(|| generated_user_default(name, &kind));
    found
        .entry(name.to_string())
        .and_modify(|variable| {
            variable.required |= required;
            if variable.default_value.is_none() {
                variable.default_value.clone_from(&default_value);
            }
        })
        .or_insert_with(|| TemplateVariable {
            name: name.to_string(),
            kind,
            required,
            default_value,
            route_service: None,
        });
}

fn is_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn variable_kind(name: &str) -> TemplateVariableKind {
    if name.starts_with("SERVICE_URL_") {
        return TemplateVariableKind::PublicUrl;
    }
    if name.starts_with("SERVICE_FQDN_") {
        return TemplateVariableKind::PublicHost;
    }
    let suffix = name.strip_prefix("SERVICE_").unwrap_or_default();
    for (prefix, kind) in [
        (
            "PASSWORDWITHSYMBOLS_64_",
            TemplateVariableKind::GeneratedPasswordWithSymbols64,
        ),
        (
            "PASSWORDWITHSYMBOLS_",
            TemplateVariableKind::GeneratedPasswordWithSymbols,
        ),
        ("PASSWORD_64_", TemplateVariableKind::GeneratedPassword64),
        ("PASSWORD_", TemplateVariableKind::GeneratedPassword),
        (
            "LOWERCASEUSER_",
            TemplateVariableKind::GeneratedLowercaseUser,
        ),
        ("USER_", TemplateVariableKind::GeneratedUser),
        ("BASE64_128_", TemplateVariableKind::GeneratedRandom128),
        ("BASE64_64_", TemplateVariableKind::GeneratedRandom64),
        ("BASE64_32_", TemplateVariableKind::GeneratedRandom32),
        ("BASE64_", TemplateVariableKind::GeneratedRandom32),
        ("REALBASE64_128_", TemplateVariableKind::GeneratedBase64_128),
        ("REALBASE64_64_", TemplateVariableKind::GeneratedBase64_64),
        ("REALBASE64_32_", TemplateVariableKind::GeneratedBase64_32),
        ("REALBASE64_", TemplateVariableKind::GeneratedBase64_32),
        ("HEX_128_", TemplateVariableKind::GeneratedHex128),
        ("HEX_64_", TemplateVariableKind::GeneratedHex64),
        ("HEX_32_", TemplateVariableKind::GeneratedHex32),
        ("SUPABASEANON_", TemplateVariableKind::GeneratedSupabaseAnon),
        (
            "SUPABASESERVICE_",
            TemplateVariableKind::GeneratedSupabaseService,
        ),
    ] {
        if suffix.starts_with(prefix) {
            return kind;
        }
    }
    TemplateVariableKind::UserInput
}

fn generated_user_default(name: &str, kind: &TemplateVariableKind) -> Option<String> {
    if !matches!(
        kind,
        TemplateVariableKind::GeneratedUser | TemplateVariableKind::GeneratedLowercaseUser
    ) {
        return None;
    }
    let name = name.to_ascii_uppercase();
    if name.contains("POSTGRES") {
        Some("postgres".to_string())
    } else if name.contains("REDIS") || name.contains("VALKEY") {
        Some("default".to_string())
    } else {
        Some("admin".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve_one_response(status: &str, body: String) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let status = status.to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request_count.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{address}/catalog.json"), requests)
    }

    fn template(compose: &str, port: Option<&str>) -> CoolifyTemplate {
        CoolifyTemplate {
            documentation: Some("https://example.com/docs".to_string()),
            slogan: Some("Example".to_string()),
            compose: base64::engine::general_purpose::STANDARD.encode(compose),
            tags: vec!["example".to_string()],
            category: Some("test".to_string()),
            logo: None,
            port: port.map(str::to_string),
            template_last_updated_at: None,
            amd_only: false,
            arm_only: false,
        }
    }

    #[test]
    fn prepares_single_service_template_for_temps() {
        let prepared = prepare_template(
            "actualbudget",
            &template(
                r#"services:
  actual_server:
    image: actualbudget/actual-server:latest
    environment:
      - SERVICE_URL_ACTUAL_5006
      - TOKEN=$SERVICE_PASSWORD_64_TOKEN
    volumes:
      - actual_data:/data
"#,
                Some("5006"),
            ),
        )
        .unwrap();

        assert!(prepared.installable());
        assert_eq!(prepared.routes.len(), 1);
        assert_eq!(prepared.routes[0].service, "actual_server");
        assert_eq!(prepared.routes[0].port, 5006);
        assert!(prepared.compose.contains("127.0.0.1::5006"));
        assert!(prepared.compose.contains("actual_data: {}"));
        assert!(prepared.variables.iter().any(|variable| {
            variable.name == "SERVICE_URL_ACTUAL_5006"
                && variable.kind == TemplateVariableKind::PublicUrl
        }));
        assert!(prepared.variables.iter().any(|variable| {
            variable.name == "SERVICE_PASSWORD_64_TOKEN"
                && variable.kind == TemplateVariableKind::GeneratedPassword64
                && variable.kind.is_secret()
        }));
    }

    #[test]
    fn http_localhost_healthchecks_use_ipv4_loopback_for_any_template() {
        let prepared = prepare_template(
            "audiobookshelf",
            &template(
                r#"services:
  audiobookshelf:
    image: ghcr.io/advplyr/audiobookshelf:2.34.0
    environment:
      - SERVICE_URL_AUDIOBOOKSHELF_80
    healthcheck:
      test:
        - CMD
        - wget
        - --quiet
        - http://localhost:80/ping
      interval: 2s
      timeout: 10s
      retries: 15
    expose:
      - 80
"#,
                Some("80"),
            ),
        )
        .expect("Audiobookshelf template should be normalized");

        let compose: YamlValue = serde_yaml::from_str(&prepared.compose).unwrap();
        let healthcheck = &compose["services"]["audiobookshelf"]["healthcheck"];
        assert!(healthcheck
            .as_mapping()
            .and_then(|mapping| mapping.get("test"))
            .and_then(YamlValue::as_sequence)
            .is_some_and(|test| test
                .iter()
                .any(|value| { value.as_str() == Some("http://127.0.0.1:80/ping") })));
        assert_eq!(
            prepared.routes[0].health_check_path.as_deref(),
            Some("/ping")
        );
        assert!(prepared
            .transformations
            .iter()
            .any(|transformation| transformation.code == "normalize_healthcheck_loopback"));
        assert!(prepared.installable());
    }

    #[test]
    fn healthcheck_loopback_normalization_rejects_hostname_lookalikes() {
        assert_eq!(
            replace_http_localhost("curl http://localhost:8080/health"),
            "curl http://127.0.0.1:8080/health"
        );
        assert_eq!(
            replace_http_localhost("wget http://localhost/health"),
            "wget http://127.0.0.1/health"
        );
        assert_eq!(
            replace_http_localhost("wget HTTP://LOCALHOST/health"),
            "wget http://127.0.0.1/health"
        );
        assert_eq!(
            replace_http_localhost("curl http://localhost.example/health"),
            "curl http://localhost.example/health"
        );
        assert_eq!(
            replace_http_localhost("curl HTTP://LOCALHOST.example/health"),
            "curl HTTP://LOCALHOST.example/health"
        );
        assert_eq!(
            replace_http_localhost("curl https://localhost/health"),
            "curl https://localhost/health"
        );
    }

    #[test]
    fn replaces_literal_template_credentials_with_generated_encrypted_variables() {
        let prepared = prepare_template(
            "supabase",
            &template(
                r#"services:
  realtime:
    image: supabase/realtime:latest
    environment:
      DB_ENC_KEY: supabaserealtime
      API_KEY_RATE_LIMIT: ${API_KEY_RATE_LIMIT:-60/minute}
  worker:
    image: example/worker
    environment:
      SHARED_SECRET: supabaserealtime
"#,
                None,
            ),
        )
        .expect("template should be normalized");

        assert!(prepared
            .compose
            .contains("DB_ENC_KEY: ${SERVICE_PASSWORD_REALTIME_DB_ENC_KEY}"));
        assert!(prepared
            .compose
            .contains("SHARED_SECRET: ${SERVICE_PASSWORD_REALTIME_DB_ENC_KEY}"));
        assert!(prepared
            .compose
            .contains("API_KEY_RATE_LIMIT: ${API_KEY_RATE_LIMIT:-60/minute}"));
        assert!(prepared.variables.iter().any(|variable| {
            variable.name == "SERVICE_PASSWORD_REALTIME_DB_ENC_KEY"
                && variable.kind == TemplateVariableKind::GeneratedPassword
        }));
        assert_eq!(
            temps_presets::validate_compose_credentials(&prepared.compose),
            Ok(())
        );
    }

    #[test]
    fn generated_credential_names_remain_distinct_after_normalization_collisions() {
        let prepared = prepare_template(
            "colliding-service-names",
            &template(
                r#"services:
  foo-bar:
    image: example/first
    environment:
      PASSWORD: first-literal
  foo_bar:
    image: example/second
    environment:
      PASSWORD: second-literal
"#,
                None,
            ),
        )
        .expect("template should allocate collision-safe generated variables");

        let generated = prepared
            .variables
            .iter()
            .filter(|variable| {
                variable
                    .name
                    .starts_with("SERVICE_PASSWORD_FOO_BAR_PASSWORD")
            })
            .map(|variable| variable.name.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(generated.len(), 2);
        assert!(generated.contains("SERVICE_PASSWORD_FOO_BAR_PASSWORD"));
        assert!(generated
            .iter()
            .any(|name| name.starts_with("SERVICE_PASSWORD_FOO_BAR_PASSWORD_")));
        for variable in generated {
            assert!(prepared.compose.contains(&format!("${{{variable}}}")));
        }
    }

    #[test]
    fn generated_credentials_do_not_reuse_existing_template_variables() {
        let prepared = prepare_template(
            "reserved-variable-name",
            &template(
                r#"services:
  existing:
    image: example/existing
    environment:
      TOKEN: ${SERVICE_PASSWORD_FOO_BAR_PASSWORD}
  foo-bar:
    image: example/generated
    environment:
      PASSWORD: generated-literal
"#,
                None,
            ),
        )
        .expect("template variable should reserve its existing name");

        let generated = prepared
            .variables
            .iter()
            .map(|variable| variable.name.as_str())
            .filter(|name| name.starts_with("SERVICE_PASSWORD_FOO_BAR_PASSWORD"))
            .collect::<BTreeSet<_>>();
        assert_eq!(generated.len(), 2);
        assert!(generated.contains("SERVICE_PASSWORD_FOO_BAR_PASSWORD"));
        assert!(generated
            .iter()
            .any(|name| *name != "SERVICE_PASSWORD_FOO_BAR_PASSWORD"));
        assert!(!prepared.compose.contains("generated-literal"));
    }

    #[test]
    fn blocks_host_affecting_templates_with_context() {
        let prepared = prepare_template(
            "socket-manager",
            &template(
                r#"services:
  manager:
    image: example/manager:1
    privileged: true
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - manager-data:/data
volumes:
  manager-data:
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::HostAccess
        );
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("privileged")));
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("/var/run/docker.sock")));
        assert!(prepared.capability_requirements.is_empty());
        assert!(!prepared
            .warnings
            .iter()
            .any(|warning| warning.contains("limited startup capabilities")));
    }

    #[test]
    fn converts_app_owned_config_paths_to_project_scoped_volumes() {
        let prepared = prepare_template(
            "apprise-api",
            &template(
                r#"services:
  apprise-api:
    image: linuxserver/apprise-api:latest
    volumes:
      - /apprise-api/config:/config
"#,
                None,
            ),
        )
        .expect("app-owned config should normalize safely");

        assert!(prepared.installable());
        assert!(!prepared.requires_host_access);
        assert!(!prepared.compose.contains("/apprise-api/config"));
        assert!(prepared.compose.contains("apprise-api-config:/config"));
        assert!(prepared
            .transformations
            .iter()
            .any(|transformation| transformation.code == "project_storage_volume"));
        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::Elevated
        );
    }

    #[test]
    fn does_not_rewrite_sensitive_or_read_only_host_config_paths() {
        for (service, mount) in [
            ("app", "/etc/example/config:/config"),
            ("app", "/app/config:/config:ro"),
            ("app", "/app/config:/config:rshared"),
            ("etc", "/etc/config:/config"),
            ("bin", "/bin/config:/config"),
            ("lib64", "/lib64/data:/data"),
            ("Users", "/Users/config:/config"),
            ("private", "/private/data:/data"),
        ] {
            let compose = format!(
                "services:\n  {service}:\n    image: example/app:1\n    volumes:\n      - {mount}\n"
            );
            let prepared = prepare_template("unsafe-config", &template(&compose, None)).unwrap();
            assert!(!prepared.installable(), "{mount}");
            assert!(prepared.requires_host_access, "{mount}");
        }
    }

    #[test]
    fn distinguishes_non_host_incompatibility_from_host_access() {
        let prepared = prepare_template(
            "missing-image",
            &template("services:\n  app:\n    command: echo unsupported\n", None),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::Blocked
        );
        assert!(!prepared.requires_host_access);
    }

    #[test]
    fn supports_multiple_public_services() {
        let prepared = prepare_template(
            "multi-ui",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - SERVICE_URL_APP_3000
  admin:
    image: example/admin:1
    environment:
      - SERVICE_URL_ADMIN_3001
"#,
                Some("3000"),
            ),
        )
        .unwrap();

        assert!(prepared.installable());
        assert_eq!(prepared.service_count, 2);
        assert_eq!(prepared.routes.len(), 2);
        assert_eq!(prepared.routes[0].service, "app");
        assert_eq!(prepared.routes[0].port, 3000);
        assert_eq!(prepared.routes[1].service, "admin");
        assert_eq!(prepared.routes[1].port, 3001);
        assert!(prepared.compose.matches("127.0.0.1::").count() >= 2);
    }

    #[test]
    fn extracts_defaults_and_user_inputs_without_treating_them_as_secrets() {
        let prepared = prepare_template(
            "configurable",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - LOG_LEVEL=${LOG_LEVEL:-info}
      - API_KEY=${API_KEY:?API key required}
"#,
                None,
            ),
        )
        .unwrap();

        let log_level = prepared
            .variables
            .iter()
            .find(|variable| variable.name == "LOG_LEVEL")
            .unwrap();
        assert_eq!(log_level.default_value.as_deref(), Some("info"));
        assert!(!log_level.required);
        let api_key = prepared
            .variables
            .iter()
            .find(|variable| variable.name == "API_KEY")
            .unwrap();
        assert!(api_key.required);
        assert_eq!(api_key.kind, TemplateVariableKind::UserInput);
    }

    #[test]
    fn keeps_internal_redis_endpoints_and_suggests_conventional_users() {
        let prepared = prepare_template(
            "budibase",
            &template(
                r#"services:
  app-service:
    image: example/app:1
    environment:
      - REDIS_URL=redis-service:6379
      - POSTGRES_USER=$SERVICE_USER_POSTGRES
      - REDIS_USER=$SERVICE_USER_REDIS
      - ADMIN_USER=$SERVICE_USER_APP
"#,
                None,
            ),
        )
        .expect("internal endpoints and generated users should normalize");

        assert!(prepared.compose.contains("REDIS_URL=redis-service:6379"));
        assert!(!prepared
            .compose
            .contains("SERVICE_PASSWORD_APP_SERVICE_REDIS_URL"));
        for (name, expected) in [
            ("SERVICE_USER_POSTGRES", "postgres"),
            ("SERVICE_USER_REDIS", "default"),
            ("SERVICE_USER_APP", "admin"),
        ] {
            let variable = prepared
                .variables
                .iter()
                .find(|variable| variable.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(variable.default_value.as_deref(), Some(expected));
        }
        assert_eq!(
            temps_presets::validate_compose_credentials(&prepared.compose),
            Ok(())
        );
    }

    #[test]
    fn plain_and_passthrough_variables_are_optional_like_docker_compose() {
        let prepared = prepare_template(
            "optional-integrations",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - COPILOT_FAL_API_KEY
      - COPILOT_OPENAI_API_KEY=${COPILOT_OPENAI_API_KEY}
      - MAILER_HOST
      - MAILER_PASSWORD=${MAILER_PASSWORD:-}
      - REQUIRED_TOKEN=${REQUIRED_TOKEN:?token required}
"#,
                None,
            ),
        )
        .unwrap();

        for name in [
            "COPILOT_FAL_API_KEY",
            "COPILOT_OPENAI_API_KEY",
            "MAILER_HOST",
            "MAILER_PASSWORD",
        ] {
            let variable = prepared
                .variables
                .iter()
                .find(|variable| variable.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert!(!variable.required, "{name} should be optional");
        }
        assert!(prepared
            .variables
            .iter()
            .find(|variable| variable.name == "REQUIRED_TOKEN")
            .is_some_and(|variable| variable.required));
    }

    #[test]
    fn route_reuses_browserless_compose_health_path() {
        let prepared = prepare_template(
            "browserless",
            &template(
                r#"services:
  browserless:
    image: ghcr.io/browserless/chromium
    environment:
      - SERVICE_URL_BROWSERLESS_3000
    expose:
      - 3000
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:3000/docs"]
"#,
                Some("3000"),
            ),
        )
        .expect("Browserless template should prepare");

        assert_eq!(prepared.routes.len(), 1);
        assert_eq!(prepared.routes[0].service, "browserless");
        assert_eq!(
            prepared.routes[0].health_check_path.as_deref(),
            Some("/docs")
        );
    }

    #[test]
    fn distinguishes_symbol_password_generators() {
        assert_eq!(
            variable_kind("SERVICE_PASSWORDWITHSYMBOLS_APP"),
            TemplateVariableKind::GeneratedPasswordWithSymbols
        );
        assert_eq!(
            variable_kind("SERVICE_PASSWORDWITHSYMBOLS_64_APP"),
            TemplateVariableKind::GeneratedPasswordWithSymbols64
        );
    }

    #[test]
    fn leaves_nested_defaults_for_compose_and_extracts_their_dependency() {
        let prepared = prepare_template(
            "nested-default",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - APP_DOMAIN=${APP_DOMAIN:-$SERVICE_FQDN_APP}
"#,
                Some("3000"),
            ),
        )
        .unwrap();

        let app_domain = prepared
            .variables
            .iter()
            .find(|variable| variable.name == "APP_DOMAIN")
            .unwrap();
        assert!(!app_domain.required);
        assert_eq!(app_domain.default_value, None);
        assert!(prepared.variables.iter().any(|variable| {
            variable.name == "SERVICE_FQDN_APP" && variable.kind == TemplateVariableKind::PublicHost
        }));
    }

    #[test]
    fn extracts_paths_assigned_to_public_url_magic_variables() {
        let prepared = prepare_template(
            "path-url",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - SERVICE_URL_APP_3000=/console
"#,
                None,
            ),
        )
        .unwrap();

        let public_url = prepared
            .variables
            .iter()
            .find(|variable| variable.name == "SERVICE_URL_APP_3000")
            .unwrap();
        assert_eq!(public_url.default_value.as_deref(), Some("/console"));
        assert_eq!(public_url.kind, TemplateVariableKind::PublicUrl);
        assert_eq!(prepared.routes[0].port, 3000);
        assert!(prepared.installable());
    }

    #[test]
    fn normalizes_fixed_ports_and_container_names() {
        let prepared = prepare_template(
            "isolated",
            &template(
                r#"name: shared-name
services:
  app:
    image: example/app:1
    container_name: shared-container
    ports:
      - "0.0.0.0:8080:3000"
    environment:
      - SERVICE_URL_APP
"#,
                None,
            ),
        )
        .unwrap();

        assert!(prepared.installable());
        assert_eq!(prepared.routes[0].port, 3000);
        assert!(prepared.compose.contains("127.0.0.1::3000"));
        assert!(!prepared.compose.contains("shared-name"));
        assert!(!prepared.compose.contains("shared-container"));
        assert!(prepared
            .transformations
            .iter()
            .any(|transformation| transformation.code == "normalize_ports"));
    }

    #[test]
    fn creates_coolify_url_and_fqdn_pairs_for_a_route() {
        let prepared = prepare_template(
            "paired-routes",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - SERVICE_URL_APP_3000=/console
"#,
                None,
            ),
        )
        .unwrap();

        for name in [
            "SERVICE_URL_APP",
            "SERVICE_FQDN_APP",
            "SERVICE_URL_APP_3000",
            "SERVICE_FQDN_APP_3000",
        ] {
            let variable = prepared
                .variables
                .iter()
                .find(|variable| variable.name == name)
                .unwrap();
            assert_eq!(variable.route_service.as_deref(), Some("app"));
        }
    }

    #[test]
    fn detects_generated_supabase_jwt_dependencies() {
        assert_eq!(
            variable_kind("SERVICE_SUPABASEANON_KEY"),
            TemplateVariableKind::GeneratedSupabaseAnon
        );
        assert_eq!(
            variable_kind("SERVICE_SUPABASESERVICE_KEY"),
            TemplateVariableKind::GeneratedSupabaseService
        );
    }

    #[test]
    fn marks_database_images_as_explicit_capability_requirements() {
        let prepared = prepare_template(
            "database-stack",
            &template(
                r#"services:
  database:
    image: postgres:17
"#,
                None,
            ),
        )
        .unwrap();

        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::Elevated
        );
        assert_eq!(prepared.capability_requirements[0].service, "database");
    }

    #[test]
    fn activepieces_uses_application_healthcheck_and_startup_permissions() {
        let prepared = prepare_template(
            "activepieces",
            &template(
                r#"services:
  activepieces:
    image: ghcr.io/activepieces/activepieces:0.75.0
    environment:
      - SERVICE_URL_ACTIVEPIECES
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:80"]
      interval: 5s
      timeout: 20s
      retries: 10
"#,
                Some("80"),
            ),
        )
        .expect("Activepieces template should prepare");

        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::Elevated
        );
        assert_eq!(prepared.capability_requirements.len(), 1);
        assert_eq!(prepared.capability_requirements[0].service, "activepieces");
        assert!(prepared.capability_requirements[0]
            .reason
            .contains("starts nginx as root"));
        assert_eq!(
            prepared.routes[0].health_check_path.as_deref(),
            Some("/api/v1/health")
        );
        assert!(prepared
            .compose
            .contains("http://127.0.0.1:80/api/v1/health"));
        assert!(prepared
            .transformations
            .iter()
            .any(|transformation| transformation.code == "normalize_healthcheck"));
    }

    #[test]
    fn activepieces_capabilities_do_not_match_untrusted_image_references() {
        for image in [
            "activepieces/activepieces.evil:0.75.0",
            "docker.io/activepieces/activepieces:0.75.0",
            "registry.example.com/activepieces/activepieces:0.75.0",
            "${ACTIVEPIECES_IMAGE}",
        ] {
            let compose = format!(
                r#"services:
  app:
    image: {image}
    environment:
      - SERVICE_URL_APP
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:80"]
"#
            );
            let prepared =
                prepare_template("untrusted-activepieces", &template(&compose, Some("80")))
                    .expect("lookalike image template should prepare without implicit permissions");

            assert!(
                prepared.capability_requirements.is_empty(),
                "{image} must not receive the Activepieces capability requirement"
            );
            assert_eq!(prepared.routes[0].health_check_path.as_deref(), Some("/"));
            assert!(!prepared
                .transformations
                .iter()
                .any(|transformation| transformation.code == "normalize_healthcheck"));
        }
    }

    #[test]
    fn requires_confirmation_for_writable_named_volume_initialization() {
        let prepared = prepare_template(
            "persistent-app",
            &template(
                r#"services:
  app:
    image: example/app:1
    volumes:
      - app_data:/data
volumes:
  app_data:
"#,
                None,
            ),
        )
        .unwrap();

        assert_eq!(
            prepared.compatibility_tier(),
            TemplateCompatibilityTier::Elevated
        );
        assert_eq!(prepared.capability_requirements[0].service, "app");
        assert!(prepared.capability_requirements[0]
            .reason
            .contains("writable Docker volume"));
    }

    #[test]
    fn read_only_and_bind_mounts_do_not_request_volume_capabilities() {
        let prepared = prepare_template(
            "read-only-mounts",
            &template(
                r#"services:
  app:
    image: example/app:1
    volumes:
      - app_data:/data:ro
      - ./config:/config
volumes:
  app_data:
"#,
                None,
            ),
        )
        .unwrap();

        assert!(prepared.capability_requirements.is_empty());
    }

    #[tokio::test]
    async fn preflight_rejects_missing_values_before_running_compose() {
        let result = preflight_template(
            "required-input",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      API_KEY: ${API_KEY:?API key required}
"#,
                None,
            ),
            &BTreeMap::new(),
            &[],
        )
        .await
        .unwrap();

        assert!(!result.ready());
        assert!(!result.compose_validated);
        assert!(result
            .errors
            .iter()
            .any(|error| error.contains("Required variable 'API_KEY'")));
    }

    #[tokio::test]
    async fn preflight_requires_startup_permission_confirmation() {
        let result = preflight_template(
            "database-stack",
            &template(
                r#"services:
  database:
    image: postgres:17
"#,
                None,
            ),
            &BTreeMap::new(),
            &[],
        )
        .await
        .unwrap();

        assert!(!result.ready());
        assert!(result
            .errors
            .iter()
            .any(|error| error.contains("requires confirmation")));
    }

    #[tokio::test]
    async fn preflight_rejects_embedded_credentials_before_project_creation() {
        let result = preflight_template(
            "literal-credential",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      DATABASE_URL: postgres://admin:literal-secret-that-must-not-be-stored@db/app
"#,
                None,
            ),
            &BTreeMap::new(),
            &[],
        )
        .await
        .unwrap();

        assert!(!result.ready());
        assert!(!result.compose_validated);
        assert!(result.errors.iter().any(|error| {
            error.contains("services.app.environment.DATABASE_URL")
                && !error.contains("literal-secret-that-must-not-be-stored")
        }));
    }

    #[tokio::test]
    async fn preflight_rejects_undeclared_and_oversized_values_without_running_compose() {
        let catalog_template = template(
            r#"services:
  app:
    image: example/app:1
    environment:
      API_KEY: ${API_KEY:?API key required}
"#,
            None,
        );
        let unknown = preflight_template(
            "strict-input",
            &catalog_template,
            &BTreeMap::from([("UNDECLARED".to_string(), "value".to_string())]),
            &[],
        )
        .await
        .unwrap_err();
        assert!(matches!(
            unknown,
            ServiceTemplateCatalogError::InvalidPreflightInput { .. }
        ));
        assert!(unknown.to_string().contains("UNDECLARED"));

        let oversized = preflight_template(
            "strict-input",
            &catalog_template,
            &BTreeMap::from([(
                "API_KEY".to_string(),
                "x".repeat(MAX_PREFLIGHT_VALUE_BYTES + 1),
            )]),
            &[],
        )
        .await
        .unwrap_err();
        assert!(matches!(
            oversized,
            ServiceTemplateCatalogError::InvalidPreflightInput { .. }
        ));
        assert!(oversized.to_string().contains("API_KEY"));
    }

    #[test]
    fn preflight_env_only_contains_declared_variables_and_escapes_values() {
        let prepared = prepare_template(
            "declared-values",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      API_KEY: ${API_KEY:?API key required}
"#,
                None,
            ),
        )
        .unwrap();
        let values = BTreeMap::from([
            ("API_KEY".to_string(), "pa\"$\nss".to_string()),
            (
                "DOCKER_HOST".to_string(),
                "tcp://attacker.invalid:2375".to_string(),
            ),
        ]);

        let rendered = render_preflight_env(&prepared.variables, &values).unwrap();

        assert_eq!(rendered, "API_KEY=\"pa\\\"$$\\nss\"\n");
        assert!(!rendered.contains("DOCKER_HOST"));
    }

    #[test]
    fn preflight_env_rejects_unsupported_control_characters() {
        let variables = vec![TemplateVariable {
            name: "API_KEY".to_string(),
            kind: TemplateVariableKind::UserInput,
            required: true,
            default_value: None,
            route_service: None,
        }];
        let values = BTreeMap::from([("API_KEY".to_string(), "secret\u{0007}".to_string())]);

        let error = render_preflight_env(&variables, &values).unwrap_err();

        assert!(error.contains("API_KEY"));
        assert!(error.contains("U+0007"));
    }

    #[test]
    fn blocks_compose_and_docker_control_variables() {
        let prepared = prepare_template(
            "control-variables",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      COMPOSE_FILE: ${COMPOSE_FILE:-docker-compose.other.yml}
      DOCKER_HOST: ${DOCKER_HOST:-tcp://docker:2375}
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("COMPOSE_FILE")));
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("DOCKER_HOST")));
    }

    #[test]
    fn accepts_null_tags_from_the_upstream_catalog() {
        let parsed: BTreeMap<String, CoolifyTemplate> = serde_json::from_str(
            r#"{"cloudflared":{"documentation":"https://example.com","slogan":"Tunnel","compose":"c2VydmljZXM6IHt9Cg==","tags":null,"category":"proxy","logo":"svgs/cloudflared.svg","port":null}}"#,
        )
        .unwrap();

        assert!(parsed["cloudflared"].tags.is_empty());
    }

    #[test]
    fn blocks_shared_docker_resources_and_missing_public_port() {
        let prepared = prepare_template(
            "external-network",
            &template(
                r#"services:
  app:
    image: example/app:1
    environment:
      - SERVICE_URL_APP
    networks:
      - shared
networks:
  shared:
    external: true
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("shared Docker resource")));
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("single routable port")));
    }

    #[test]
    fn blocks_environment_expansion_in_host_mount_sources() {
        let prepared = prepare_template(
            "host-env-mount",
            &template(
                r#"services:
  app:
    image: example/app:1
    volumes:
      - $HOME:/host-home
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("$HOME")));
    }

    #[test]
    fn blocks_absolute_and_relative_label_files() {
        for label_file in ["/etc/shadow", "./labels.env"] {
            let prepared = prepare_template(
                "label-file",
                &template(
                    &format!(
                        r#"services:
  app:
    image: example/app:1
    label_file: {label_file}
"#
                    ),
                    None,
                ),
            )
            .unwrap();

            assert!(!prepared.installable());
            assert!(prepared
                .compatibility_issues
                .iter()
                .any(|issue| issue.contains("label_file")));
        }
    }

    #[test]
    fn blocks_interpolation_in_deployer_guarded_fields() {
        let prepared = prepare_template(
            "guarded-interpolation",
            &template(
                r#"services:
  app:
    image: example/app:1
    privileged: ${PRIVILEGED:-false}
    shm_size: ${SHM_SIZE:-64m}
    volumes:
      - type: volume
        source: data
        target: ${DATA_TARGET:-/data}
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        for field in ["privileged", "shm_size", "volumes"] {
            assert!(prepared
                .compatibility_issues
                .iter()
                .any(|issue| issue.contains(field) && issue.contains("interpolates")));
        }
    }

    #[test]
    fn install_plan_digest_is_stable_and_binds_compose_and_metadata() {
        let first_template = template(
            "services:\n  app:\n    image: example/app:1\n",
            Some("3000"),
        );
        let first = prepare_template("digest", &first_template).unwrap();
        let same_template = first_template.clone();
        let same = prepare_template("digest", &same_template).unwrap();
        let changed_template = template(
            "services:\n  app:\n    image: example/app:2\n",
            Some("3000"),
        );
        let changed = prepare_template("digest", &changed_template).unwrap();
        let metadata_changed_template = template(
            "services:\n  app:\n    image: example/app:1\n",
            Some("4000"),
        );
        let metadata_changed = prepare_template("digest", &metadata_changed_template).unwrap();

        assert_eq!(
            first.install_plan_digest(&first_template),
            same.install_plan_digest(&same_template)
        );
        assert_ne!(
            first.install_plan_digest(&first_template),
            changed.install_plan_digest(&changed_template)
        );
        assert_ne!(
            first.install_plan_digest(&first_template),
            metadata_changed.install_plan_digest(&metadata_changed_template)
        );
        assert_eq!(first.install_plan_digest(&first_template).len(), 64);
    }

    #[test]
    fn discovers_bundled_backing_service_families() {
        let prepared = prepare_template(
            "dependencies",
            &template(
                r#"services:
  db:
    image: registry.example.test:5000/postgres:17
  cache:
    image: valkey/valkey:8
  documents:
    image: mongo:8
  objects:
    image: minio/minio:latest
  app:
    image: example/app:1
"#,
                None,
            ),
        )
        .unwrap();

        assert_eq!(
            prepared
                .backing_services
                .iter()
                .map(|service| (service.service.as_str(), service.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("cache", "redis"),
                ("db", "postgres"),
                ("documents", "mongodb"),
                ("objects", "s3"),
            ]
        );
    }

    #[test]
    fn backing_service_detection_does_not_match_unrelated_names() {
        for image in [
            "example/postgres-exporter:latest",
            "example/redis-commander:latest",
            "example/mongodb-ui:latest",
            "example/minio-client:latest",
        ] {
            assert_eq!(classify_backing_service_image(image), None, "{image}");
        }
    }

    #[test]
    fn credential_like_user_inputs_are_backend_classified_as_secrets() {
        for name in [
            "APP_KEY",
            "PUSH_SERVICE_KEY",
            "SERVICE_OPENAI_KEY",
            "SERVICE_BEEHIIVE_KEY",
            "STRIPE_SIGNING_KEY",
            "STRIPE_SIGNING_KEY_CONNECT",
            "CERT_PASSPHRASE",
            "NEXT_PRIVATE_SIGNING_LOCAL_FILE_PASSPHRASE",
            "MAIL_OPTIONS_AUTH_PASS",
            "BACKEND_MAIL_AUTH_PASS",
            "SPARKY_FITNESS_EMAIL_PASS",
            "NTFY_SMTP_SENDER_PASS",
        ] {
            let variable = TemplateVariable {
                name: name.to_string(),
                kind: TemplateVariableKind::UserInput,
                required: true,
                default_value: None,
                route_service: None,
            };
            assert!(variable.is_secret(), "{name} should be write-only");
        }

        for name in [
            "NEXT_PUBLIC_API_KEY",
            "STRIPE_PUBLISHABLE_KEY",
            "SUPABASE_ANON_KEY",
            "SERVICE_FQDN_APP",
            "PASSPHRASE_FILE",
        ] {
            let variable = TemplateVariable {
                name: name.to_string(),
                kind: TemplateVariableKind::UserInput,
                required: false,
                default_value: None,
                route_service: None,
            };
            assert!(!variable.is_secret(), "{name} should remain readable");
        }
    }

    #[tokio::test]
    async fn stale_snapshot_is_served_during_failed_refresh_backoff() {
        let catalog = ServiceTemplateCatalog::with_client(
            reqwest::Client::new(),
            "http://127.0.0.1:9/unreachable".to_string(),
        );
        *catalog.cache.write().await = Some(Arc::new(CatalogSnapshot {
            templates: BTreeMap::new(),
            analyses: BTreeMap::new(),
            fetched_at: Utc::now(),
            etag: Some("stale".to_string()),
            refreshed_at: Instant::now() - CATALOG_TTL - Duration::from_secs(1),
        }));
        *catalog.last_failed_refresh.write().await = Some((
            Instant::now(),
            ServiceTemplateCatalogError::Fetch {
                url: "http://127.0.0.1:9/unreachable".to_string(),
                reason: "offline".to_string(),
            },
        ));

        let snapshot = catalog.snapshot().await.unwrap();

        assert_eq!(snapshot.etag.as_deref(), Some("stale"));
    }

    #[tokio::test]
    async fn fetched_catalog_is_analyzed_and_reused_from_cache() {
        let compose = base64::engine::general_purpose::STANDARD
            .encode("services:\n  app:\n    image: example/app:1\n");
        let body = serde_json::json!({
            "example": {
                "documentation": "https://example.com/docs",
                "slogan": "Example",
                "compose": compose,
                "tags": ["example"],
                "category": "test",
                "port": "3000"
            }
        })
        .to_string();
        let (source_url, requests) = serve_one_response("200 OK", body).await;
        let catalog = ServiceTemplateCatalog::with_client(reqwest::Client::new(), source_url);

        let first = catalog.snapshot().await.unwrap();
        let second = catalog.snapshot().await.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let analysis = &first.analyses["example"];
        assert_eq!(analysis.service_count, 1);
        assert!(analysis.installable);
    }

    #[tokio::test]
    async fn failed_refresh_sets_backoff_and_serves_stale_cache() {
        let (source_url, requests) =
            serve_one_response("500 Internal Server Error", String::new()).await;
        let catalog = ServiceTemplateCatalog::with_client(reqwest::Client::new(), source_url);
        let stale = Arc::new(CatalogSnapshot {
            templates: BTreeMap::new(),
            analyses: BTreeMap::new(),
            fetched_at: Utc::now(),
            etag: Some("stale".to_string()),
            refreshed_at: Instant::now() - CATALOG_TTL - Duration::from_secs(1),
        });
        *catalog.cache.write().await = Some(stale.clone());

        let first = catalog.snapshot().await.unwrap();
        let second = catalog.snapshot().await.unwrap();

        assert!(Arc::ptr_eq(&first, &stale));
        assert!(Arc::ptr_eq(&second, &stale));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(catalog.refresh_backoff_active().await);
    }

    #[tokio::test]
    async fn concurrent_cold_cache_failures_fetch_once_during_backoff() {
        let (source_url, requests) =
            serve_one_response("500 Internal Server Error", String::new()).await;
        let catalog = Arc::new(ServiceTemplateCatalog::with_client(
            reqwest::Client::new(),
            source_url,
        ));

        let (first, second) = tokio::join!(catalog.snapshot(), catalog.snapshot());

        assert!(matches!(
            first,
            Err(ServiceTemplateCatalogError::HttpStatus { status: 500, .. })
        ));
        assert!(matches!(
            second,
            Err(ServiceTemplateCatalogError::HttpStatus { status: 500, .. })
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(catalog.refresh_backoff_active().await);
    }
}
