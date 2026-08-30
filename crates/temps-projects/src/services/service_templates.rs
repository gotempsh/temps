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
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, Semaphore};

pub const COOLIFY_CATALOG_URL: &str =
    "https://cdn.coollabs.io/coolify/service-templates-latest.json";
pub const COOLIFY_REPOSITORY_URL: &str = "https://github.com/coollabsio/coolify";

const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);
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

#[derive(Debug, Error)]
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
    pub fetched_at: DateTime<Utc>,
    pub etag: Option<String>,
    refreshed_at: Instant,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRoute {
    pub service: String,
    pub port: u16,
    pub variable_names: Vec<String>,
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
    Blocked,
}

impl TemplateCompatibilityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Elevated => "elevated",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedServiceTemplate {
    pub compose: String,
    pub service_count: usize,
    pub routes: Vec<TemplateRoute>,
    pub variables: Vec<TemplateVariable>,
    pub compatibility_issues: Vec<String>,
    pub warnings: Vec<String>,
    pub transformations: Vec<TemplateTransformation>,
    pub capability_requirements: Vec<TemplateCapabilityRequirement>,
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
            TemplateCompatibilityTier::Blocked
        } else if self.capability_requirements.is_empty() {
            TemplateCompatibilityTier::Standard
        } else {
            TemplateCompatibilityTier::Elevated
        }
    }
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
                "Service '{}' requires explicit approval for limited startup capabilities: {}",
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
                "Capability approval for service '{service}' is unnecessary and will be ignored"
            ));
        }
    }

    errors.sort();
    errors.dedup();
    warnings.sort();
    warnings.dedup();
    let compose_validated = if errors.is_empty() {
        let _permit = PREFLIGHT_COMPOSE_SLOTS.try_acquire().map_err(|_| {
            ServiceTemplateCatalogError::PreflightBusy {
                slug: slug.to_string(),
                limit: MAX_CONCURRENT_PREFLIGHTS,
            }
        })?;
        match validate_with_docker_compose(slug, &prepared.compose, &prepared.variables, values)
            .await
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
) -> Result<(), String> {
    let directory = tempfile::Builder::new()
        .prefix("temps-service-template-preflight-")
        .tempdir()
        .map_err(|error| {
            format!("Could not create a temporary preflight directory for '{slug}': {error}")
        })?;
    let compose_path = directory.path().join("docker-compose.yml");
    tokio::fs::write(&compose_path, compose)
        .await
        .map_err(|error| {
            format!(
                "Could not write the temporary Compose file for template '{slug}' at '{}': {error}",
                compose_path.display()
            )
        })?;
    create_preflight_env_files(directory.path(), compose, slug).await?;
    let env_content = render_preflight_env(variables, values)?;
    tokio::fs::write(directory.path().join(".env"), env_content)
        .await
        .map_err(|error| {
            format!("Could not create the temporary .env for template '{slug}': {error}")
        })?;

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
        .map_err(|_| {
            format!("Docker Compose validation for template '{slug}' timed out after 15 seconds")
        })?
        .map_err(|error| {
            format!("Docker Compose is unavailable while validating template '{slug}': {error}")
        })?;
    if output.status.success() {
        return Ok(());
    }
    let mut diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
    for value in values.values().filter(|value| !value.is_empty()) {
        diagnostic = diagnostic.replace(value, "***");
    }
    diagnostic.truncate(diagnostic.floor_char_boundary(8 * 1024));
    Err(format!(
        "Docker Compose rejected template '{slug}': {}",
        diagnostic.trim()
    ))
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
        }
    }

    pub async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, ServiceTemplateCatalogError> {
        if let Some(snapshot) = self.fresh_snapshot().await {
            return Ok(snapshot);
        }

        let _refresh_guard = self.refresh_guard.lock().await;
        if let Some(snapshot) = self.fresh_snapshot().await {
            return Ok(snapshot);
        }

        let stale = self.cache.read().await.clone();
        match self.fetch_catalog().await {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                *self.cache.write().await = Some(snapshot.clone());
                Ok(snapshot)
            }
            Err(error) => {
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

        Ok(CatalogSnapshot {
            templates,
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
    let mut compatibility_issues = compatibility_issues(&root);
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
    let variables = extract_variables(&root, &routes);
    for variable in variables.iter().filter(|variable| {
        variable.name.starts_with("COMPOSE_") || variable.name.starts_with("DOCKER_")
    }) {
        compatibility_issues.push(format!(
            "Variable '{}' can alter Docker Compose control-plane behavior",
            variable.name
        ));
    }
    let capability_requirements = capability_requirements(&root);
    if !capability_requirements.is_empty() {
        warnings.push(
            "Some images commonly need limited startup capabilities to initialize persistent data; explicit approval is required before installation"
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
        routes,
        variables,
        compatibility_issues,
        warnings,
        transformations,
        capability_requirements,
    })
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
            (database || persistent_volume).then(|| TemplateCapabilityRequirement {
                service: name.to_string(),
                capability: "relaxed_linux_capabilities",
                reason: if database {
                    "This image commonly initializes persistent data as root before dropping to its runtime user"
                        .to_string()
                } else {
                    "This service has a writable Docker volume; some images need limited ownership capabilities to initialize the empty volume"
                        .to_string()
                },
            })
        })
        .collect()
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
                                true,
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
                            insert_variable(&mut found, key, None, true);
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
                let required = operator.starts_with(":?")
                    || operator.starts_with('?')
                    || default_expression.is_none();
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
            insert_variable(found, name, None, true);
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

#[cfg(test)]
mod tests {
    use super::*;

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
"#,
                None,
            ),
        )
        .unwrap();

        assert!(!prepared.installable());
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("privileged")));
        assert!(prepared
            .compatibility_issues
            .iter()
            .any(|issue| issue.contains("/var/run/docker.sock")));
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
    fn requires_approval_for_writable_named_volume_initialization() {
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
    async fn preflight_requires_explicit_capability_approval() {
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
            .any(|error| error.contains("explicit approval")));
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
}
