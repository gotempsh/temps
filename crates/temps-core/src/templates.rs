// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project Templates Configuration
//!
//! Curated project templates that users can use to quickly create new projects.
//! Bundled templates are individual YAML files grouped by template kind.
//! Operators may still provide a single catalog file for local customization.

use include_dir::{include_dir, Dir};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use utoipa::ToSchema;

/// Environment variable template definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct EnvVarTemplate {
    /// Name of the environment variable
    pub name: String,
    /// Example value for documentation
    #[serde(default)]
    pub example: Option<String>,
    /// Default value if not provided by user
    #[serde(default)]
    pub default: Option<String>,
    /// Description of what this variable is used for
    #[serde(default)]
    pub description: Option<String>,
    /// Whether this variable is required
    #[serde(default)]
    pub required: bool,
    /// Explicit sensitivity classification for credentials whose names do not
    /// match the conservative built-in heuristic.
    #[serde(default)]
    pub secret: bool,
    /// Frontend-side generator for the default value. Recognised values:
    /// `app_url` (https://{repo}.{base_domain}), `random_secret` (32-byte base64),
    /// `random_hex_32` (32-byte hex). Unknown values are ignored client-side.
    #[serde(default)]
    pub default_generator: Option<String>,
}

impl EnvVarTemplate {
    /// Whether values for this template input must use the secret reveal path.
    ///
    /// Templates do not get to weaken this policy by omission: credential-like
    /// names and secret generators are always treated as sensitive. Ordinary
    /// configuration is still encrypted at rest, but remains readable in the
    /// project environment-variable UI.
    pub fn is_secret(&self) -> bool {
        if self.secret {
            return true;
        }
        if self
            .default_generator
            .as_deref()
            .is_some_and(|generator| generator.contains("secret"))
        {
            return true;
        }

        let key = self.name.to_ascii_uppercase();
        let secret_segment = key.split('_').any(|segment| {
            matches!(
                segment,
                "SECRET" | "PASSWORD" | "PASSWD" | "TOKEN" | "PRIVATEKEY"
            )
        });
        secret_segment
            || key.ends_with("_API_KEY")
            || key.ends_with("_PRIVATE_KEY")
            || key.ends_with("_ACCESS_KEY")
            || key.ends_with("_DATABASE_URL")
            || key.ends_with("_POSTGRES_URL")
            || key.ends_with("_MYSQL_URL")
            || key.ends_with("_MONGODB_URL")
            || key.ends_with("_MONGODB_URI")
            || key.ends_with("_REDIS_URL")
            || key.ends_with("_AMQP_URL")
            || key.ends_with("_CONNECTION_STRING")
            || key.ends_with("_DSN")
            || key.ends_with("_WEBHOOK_URL")
            || matches!(
                key.as_str(),
                "DATABASE_URL"
                    | "POSTGRES_URL"
                    | "MYSQL_URL"
                    | "MONGODB_URL"
                    | "MONGODB_URI"
                    | "REDIS_URL"
                    | "AMQP_URL"
                    | "CONNECTION_STRING"
                    | "DSN"
                    | "WEBHOOK_URL"
            )
    }
}

/// Git repository reference (supports any git provider: GitHub, GitLab, Bitbucket, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct GitRef {
    /// Git repository URL (e.g., "https://github.com/owner/repo.git" or "https://gitlab.com/owner/repo.git")
    pub url: String,
    /// Path within the repository (for monorepos)
    /// Also accepts "subfolder" as an alias in YAML/JSON
    #[serde(default, alias = "subfolder")]
    pub path: Option<String>,
    /// Git reference (branch, tag, or commit)
    #[serde(default = "default_ref")]
    pub r#ref: String,
}

fn default_ref() -> String {
    "main".to_string()
}

fn default_true() -> bool {
    true
}

/// Where a template is presented in the project creation flow.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    /// Source-code starter shown in the regular template gallery.
    #[default]
    Starter,
    /// Curated application service shown in the service gallery.
    Service,
}

/// Resource profile required by a curated template. CPU values use the same
/// microcore unit as project deployment configuration; memory values are MiB.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TemplateResources {
    #[serde(default)]
    pub cpu_request: Option<i32>,
    #[serde(default)]
    pub cpu_limit: Option<i32>,
    #[serde(default)]
    pub memory_request: Option<i32>,
    #[serde(default)]
    pub memory_limit: Option<i32>,
}

/// A curated project template
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ProjectTemplate {
    /// Unique identifier for the template (used in URLs)
    pub slug: String,
    /// Display name
    pub name: String,
    /// Version of this template release. Service projects pin this value and
    /// the complete resolved definition so catalog updates are always an
    /// explicit, reviewable upgrade rather than a silent runtime mutation.
    #[serde(default)]
    pub version: String,
    /// Gallery this template belongs to. Older configurations default to a
    /// source-code starter, preserving their existing behaviour.
    #[serde(default)]
    pub kind: TemplateKind,
    /// Short description
    #[serde(default)]
    pub description: Option<String>,
    /// URL to template image/icon
    #[serde(default)]
    pub image_url: Option<String>,
    /// URL to a full screenshot/banner preview of the deployed template (e.g.
    /// `/templates/nextjs-saas-starter.png`). Rendered as a wide preview on the
    /// template card; optional — templates without one show no banner.
    #[serde(default)]
    pub screenshot_url: Option<String>,
    /// Git repository reference (supports any git provider). Always present as
    /// the source-of-truth / build fallback, even for image-based templates.
    pub git: GitRef,
    /// Framework/preset to use (e.g., "nextjs", "fastapi", "dockerfile")
    pub preset: String,
    /// Preset-specific configuration
    #[serde(default)]
    pub preset_config: Option<serde_json::Value>,
    /// Prebuilt Docker image reference (e.g. "ghcr.io/org/app:latest"). When set,
    /// the one-click deploy pulls and runs this image directly (source_type
    /// docker_image) instead of building from `git` — instant, no BuildKit. When
    /// absent, the template builds from source.
    #[serde(default)]
    pub image: Option<String>,
    /// Optional command passed to the container image. This is needed for
    /// production images whose default command is intentionally a development
    /// mode (for example Keycloak).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Minimum/safe runtime resources for this application. These values are
    /// validated against operator-configured tenant ceilings before creation.
    #[serde(default)]
    pub resources: Option<TemplateResources>,
    /// Container port the prebuilt image listens on (used for routing when
    /// deploying from `image`). Falls back to the image's EXPOSE / 3000 default.
    #[serde(default)]
    pub exposed_port: Option<i32>,
    /// HTTP health-check path probed after the container starts (image deploys
    /// can't read `.temps.yaml`). Must start with '/'. Defaults to "/".
    #[serde(default)]
    pub health_check_path: Option<String>,
    /// Tags/categories for filtering
    #[serde(default)]
    pub tags: Vec<String>,
    /// Feature highlights
    #[serde(default)]
    pub features: Vec<String>,
    /// Required external services (e.g., ["postgres", "redis"])
    #[serde(default)]
    pub services: Vec<String>,
    /// Environment aliases populated from a linked managed service. The outer
    /// key is the Temps service type and each inner entry maps an application
    /// variable to a variable supplied by that service.
    ///
    /// Example: `postgres.KC_DB_USERNAME: POSTGRES_USER`.
    #[serde(default)]
    pub managed_service_bindings: BTreeMap<String, BTreeMap<String, String>>,
    /// Environment variables template
    #[serde(default)]
    pub env_vars: Vec<EnvVarTemplate>,
    /// Whether the template is publicly visible
    #[serde(default = "default_true")]
    pub is_public: bool,
    /// Whether the template is featured/promoted
    #[serde(default)]
    pub is_featured: bool,
    /// Sort order for display (lower = first)
    #[serde(default)]
    pub sort_order: i32,
}

/// Immutable template release attached to a service project.
///
/// The resolved definition is deliberately stored with the project. The live
/// catalog is only needed to discover a newer release; deployments, edits and
/// rollbacks continue to work if the catalog later changes or disappears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ServiceTemplateInstance {
    /// Catalog schema used to deserialize `template`.
    pub schema_version: String,
    /// Stable service family identifier.
    pub slug: String,
    /// Applied template release.
    pub version: String,
    /// Exact resolved release from which this project was created/upgraded.
    pub template: ProjectTemplate,
}

/// Snapshot schema understood by this binary. A future schema must introduce
/// an explicit migration/decoder before projects can persist it.
pub const SERVICE_TEMPLATE_SCHEMA_VERSION: &str = "2";

#[derive(Debug, thiserror::Error)]
pub enum ServiceTemplateInstanceError {
    #[error("unsupported service template schema '{found}'; expected '{expected}'")]
    UnsupportedSchema {
        found: String,
        expected: &'static str,
    },
    #[error("service template release identity does not match its resolved definition")]
    IdentityMismatch,
    #[error("service template {slug}@{version} has an invalid release version: {source}")]
    InvalidVersion {
        slug: String,
        version: String,
        #[source]
        source: semver::Error,
    },
    #[error("service template {slug}@{version} has an invalid resolved definition: {reason}")]
    InvalidDefinition {
        slug: String,
        version: String,
        reason: String,
    },
}

impl ServiceTemplateInstance {
    pub fn new(schema_version: impl Into<String>, template: ProjectTemplate) -> Self {
        Self {
            schema_version: schema_version.into(),
            slug: template.slug.clone(),
            version: template.version.clone(),
            template,
        }
    }

    /// Parse the persisted release identifier using Semantic Versioning.
    pub fn release_version(&self) -> Result<semver::Version, semver::Error> {
        semver::Version::parse(&self.version)
    }

    pub fn validate(&self) -> Result<(), ServiceTemplateInstanceError> {
        if self.schema_version != SERVICE_TEMPLATE_SCHEMA_VERSION {
            return Err(ServiceTemplateInstanceError::UnsupportedSchema {
                found: self.schema_version.clone(),
                expected: SERVICE_TEMPLATE_SCHEMA_VERSION,
            });
        }
        if self.slug != self.template.slug || self.version != self.template.version {
            return Err(ServiceTemplateInstanceError::IdentityMismatch);
        }
        self.release_version()
            .map_err(|source| ServiceTemplateInstanceError::InvalidVersion {
                slug: self.slug.clone(),
                version: self.version.clone(),
                source,
            })?;

        let mut errors = TemplateService::validate_template(&self.template);
        if self.template.kind != TemplateKind::Service {
            errors.push("Resolved definition must have kind 'service'".to_string());
        }
        if !errors.is_empty() {
            return Err(ServiceTemplateInstanceError::InvalidDefinition {
                slug: self.slug.clone(),
                version: self.version.clone(),
                reason: errors.join("; "),
            });
        }

        Ok(())
    }

    /// Whether this release is a strictly newer release of the same service.
    pub fn is_newer_than(&self, applied: &Self) -> Result<bool, semver::Error> {
        Ok(self.slug == applied.slug && self.release_version()? > applied.release_version()?)
    }
}

/// Root configuration for templates
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TemplatesConfig {
    /// Version of the configuration schema
    #[serde(default = "default_version")]
    pub version: String,
    /// List of project templates
    #[serde(default)]
    pub templates: Vec<ProjectTemplate>,
}

fn default_version() -> String {
    "1".to_string()
}

impl TemplatesConfig {
    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Serialize configuration to YAML string
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    /// Load configuration from a file path
    pub fn from_file(path: &Path) -> Result<Self, TemplateConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            TemplateConfigError::IoError(format!("Failed to read file {:?}: {}", path, e))
        })?;
        Self::from_yaml(&content)
            .map_err(|e| TemplateConfigError::ParseError(format!("Failed to parse YAML: {}", e)))
    }

    /// Get all public templates
    pub fn public_templates(&self) -> Vec<&ProjectTemplate> {
        self.templates.iter().filter(|t| t.is_public).collect()
    }

    /// Get a template by slug
    pub fn get_by_slug(&self, slug: &str) -> Option<&ProjectTemplate> {
        self.templates.iter().find(|t| t.slug == slug)
    }

    /// Get all unique tags
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .templates
            .iter()
            .filter(|t| t.is_public)
            .flat_map(|t| t.tags.clone())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

/// Known valid services that templates can depend on
pub const VALID_SERVICES: &[&str] = &[
    "postgres", "mariadb", "redis", "mongodb", "s3", "kv", "blob", "rustfs",
];

/// Canonical compatibility family for a managed-service type.
///
/// RustFS implements the S3 API, so templates that require object storage may
/// use either the native S3 service or RustFS. Historical aliases are accepted
/// here as well so every API, deployment, and UI path applies the same rule.
pub fn canonical_managed_service_type(service_type: &str) -> String {
    match service_type.trim().to_ascii_lowercase().as_str() {
        "postgresql" => "postgres".to_string(),
        "mysql" => "mariadb".to_string(),
        "object_storage" | "object-storage" | "rustfs" => "s3".to_string(),
        normalized => normalized.to_string(),
    }
}

pub fn managed_service_types_compatible(required: &str, selected: &str) -> bool {
    canonical_managed_service_type(required) == canonical_managed_service_type(selected)
}

fn is_pinned_image_reference(image: &str) -> bool {
    image.rsplit_once('@').is_some_and(|(name, digest)| {
        !name.trim().is_empty()
            && digest.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit())
            })
    })
}

/// Presets accepted by project creation. Kept here so invalid bundled YAML is
/// rejected before a user reaches the install form.
pub const VALID_PRESETS: &[&str] = &[
    "nextjs",
    "vite",
    "astro",
    "nuxt",
    "remix",
    "sveltekit",
    "solidstart",
    "angular",
    "vue",
    "react",
    "docusaurus",
    "rsbuild",
    "python",
    "fastapi",
    "flask",
    "django",
    "rails",
    "go",
    "rust",
    "java",
    "laravel",
    "dockerfile",
    "nixpacks",
    "autopack",
    "static",
    "docker-compose",
    "nodejs",
];

/// Validation error for a single template
#[derive(Debug, Clone)]
pub struct TemplateValidationError {
    pub slug: String,
    pub errors: Vec<String>,
}

impl std::fmt::Display for TemplateValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Template '{}': {}", self.slug, self.errors.join(", "))
    }
}

/// Error type for template configuration
#[derive(Debug, Clone, thiserror::Error)]
pub enum TemplateConfigError {
    #[error("IO error: {0}")]
    IoError(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Template not found: {0}")]
    NotFound(String),
    #[error("Validation errors: {0:?}")]
    ValidationErrors(Vec<TemplateValidationError>),
}

/// Bundled templates are embedded recursively so adding a catalog entry only
/// requires a new YAML file. Directory names are part of the schema: starters
/// live in `templates/starters` and native services in `templates/services`.
static BUNDLED_TEMPLATE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

static BUNDLED_TEMPLATES: Lazy<Result<TemplatesConfig, TemplateConfigError>> =
    Lazy::new(load_bundled_templates);

fn collect_bundled_yaml_files(
    directory: &'static Dir<'static>,
    files: &mut Vec<(&'static str, &'static str)>,
) -> Result<(), TemplateConfigError> {
    for file in directory.files() {
        if file
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("yaml")
        {
            continue;
        }
        let path = file.path().to_str().ok_or_else(|| {
            TemplateConfigError::ParseError(
                "Bundled template path contains invalid UTF-8".to_string(),
            )
        })?;
        let yaml = file.contents_utf8().ok_or_else(|| {
            TemplateConfigError::ParseError(format!(
                "Bundled template '{path}' contains invalid UTF-8"
            ))
        })?;
        files.push((path, yaml));
    }
    for child in directory.dirs() {
        collect_bundled_yaml_files(child, files)?;
    }
    Ok(())
}

fn load_bundled_templates() -> Result<TemplatesConfig, TemplateConfigError> {
    let mut files = Vec::new();
    collect_bundled_yaml_files(&BUNDLED_TEMPLATE_DIR, &mut files)?;
    parse_bundled_templates(files)
}

fn parse_bundled_templates(
    mut files: Vec<(&str, &str)>,
) -> Result<TemplatesConfig, TemplateConfigError> {
    files.sort_unstable_by_key(|(path, _)| *path);

    let mut templates = Vec::with_capacity(files.len());
    let mut slug_sources = BTreeMap::new();
    for (path, yaml) in files {
        let template: ProjectTemplate = serde_yaml::from_str(yaml).map_err(|error| {
            TemplateConfigError::ParseError(format!(
                "Failed to parse bundled template '{path}': {error}"
            ))
        })?;
        let expected_kind = if path.starts_with("services/") {
            TemplateKind::Service
        } else if path.starts_with("starters/") {
            TemplateKind::Starter
        } else {
            return Err(TemplateConfigError::ParseError(format!(
                "Bundled template '{path}' must be placed under templates/services or templates/starters"
            )));
        };
        if template.kind != expected_kind {
            return Err(TemplateConfigError::ParseError(format!(
                "Bundled template '{}' declares kind '{:?}' but is stored in '{}'",
                template.slug, template.kind, path
            )));
        }
        let file_stem = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                TemplateConfigError::ParseError(format!(
                    "Bundled template '{path}' does not have a valid UTF-8 filename"
                ))
            })?;
        if file_stem != template.slug {
            return Err(TemplateConfigError::ParseError(format!(
                "Bundled template '{path}' must use its slug '{}' as the filename",
                template.slug
            )));
        }
        if let Some(previous_path) = slug_sources.insert(template.slug.clone(), path) {
            return Err(TemplateConfigError::ParseError(format!(
                "Bundled template slug '{}' is duplicated in '{previous_path}' and '{path}'",
                template.slug
            )));
        }
        templates.push(template);
    }

    if templates.is_empty() {
        return Err(TemplateConfigError::ParseError(
            "Bundled template directory contains no YAML templates".to_string(),
        ));
    }

    let config = TemplatesConfig {
        version: SERVICE_TEMPLATE_SCHEMA_VERSION.to_string(),
        templates,
    };
    let validation_errors = TemplateService::validate_config(&config);
    if validation_errors.is_empty() {
        Ok(config)
    } else {
        Err(TemplateConfigError::ValidationErrors(validation_errors))
    }
}

fn bundled_templates_config() -> Result<&'static TemplatesConfig, TemplateConfigError> {
    BUNDLED_TEMPLATES.as_ref().map_err(Clone::clone)
}

/// Maximum template slug length accepted by the catalog and project schema.
pub const MAX_TEMPLATE_SLUG_CHARS: usize = 255;

/// Return a bundled template only when the slug is part of the embedded,
/// reviewed catalog. Runtime configuration files cannot influence this lookup.
/// This is public solely for cross-crate test fixtures; production code must
/// resolve templates through [`TemplateService`].
#[doc(hidden)]
pub fn bundled_template_by_slug(slug: &str) -> Option<ProjectTemplate> {
    bundled_templates_config()
        .ok()?
        .templates
        .iter()
        .find(|template| template.slug == slug)
        .cloned()
}

/// Stored provenance marker for projects created from an operator-defined
/// template. The operator's actual slug stays local to the template catalog.
pub const CUSTOM_TEMPLATE_PROVENANCE: &str = "custom";

/// Return the slug only when it is a fixed label from the bundled catalog.
pub fn telemetry_safe_template_slug(slug: &str) -> Option<&str> {
    bundled_templates_config()
        .ok()?
        .templates
        .iter()
        .any(|template| template.slug == slug && template.is_public)
        .then_some(slug)
}

/// Return the public bundled slug only when the selected template exactly
/// matches its embedded catalog definition.
///
/// External template files can replace the bundled catalog and may reuse one
/// of its slugs. Comparing the complete definition prevents such an override
/// from being attributed to the bundled template merely because its name
/// matches a public label.
pub fn bundled_telemetry_template_slug(template: &ProjectTemplate) -> Option<&str> {
    telemetry_safe_template_slug(&template.slug)?;
    let bundled = bundled_templates_config().ok()?;
    bundled
        .templates
        .iter()
        .any(|candidate| candidate == template)
        .then_some(template.slug.as_str())
}

/// Produce the bounded value persisted on a template-created project.
pub fn template_provenance(template: &ProjectTemplate) -> &str {
    bundled_telemetry_template_slug(template).unwrap_or(CUSTOM_TEMPLATE_PROVENANCE)
}

/// Template service that manages loading and caching templates
pub struct TemplateService {
    config: Arc<RwLock<TemplatesConfig>>,
    config_path: Option<std::path::PathBuf>,
}

impl TemplateService {
    /// Create a new template service with an optional config file path
    /// Bundled templates are loaded automatically; external file can override them
    pub fn new(config_path: Option<std::path::PathBuf>) -> Result<Self, TemplateConfigError> {
        let config = bundled_templates_config()?.clone();
        info!("Loaded {} bundled templates", config.templates.len());

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            config_path,
        })
    }

    /// Load templates from the configured file path (overrides bundled templates)
    pub async fn load(&self) -> Result<(), TemplateConfigError> {
        let Some(path) = &self.config_path else {
            debug!("No external templates config path configured, using bundled templates");
            return Ok(());
        };

        if !path.exists() {
            debug!(
                "External templates config file not found at {:?}, using bundled templates",
                path
            );
            return Ok(());
        }

        info!("Loading templates from external file {:?}", path);
        let config = TemplatesConfig::from_file(path)?;
        if !matches!(
            config.version.as_str(),
            "1" | SERVICE_TEMPLATE_SCHEMA_VERSION
        ) {
            return Err(TemplateConfigError::ParseError(format!(
                "Unsupported template catalog schema '{}'; supported versions are '1' and '{}'",
                config.version, SERVICE_TEMPLATE_SCHEMA_VERSION
            )));
        }
        let contains_service_templates = config
            .templates
            .iter()
            .any(|template| template.kind == TemplateKind::Service);
        if contains_service_templates && config.version != SERVICE_TEMPLATE_SCHEMA_VERSION {
            return Err(TemplateConfigError::ParseError(format!(
                "Service templates require catalog schema '{}'; received '{}'",
                SERVICE_TEMPLATE_SCHEMA_VERSION, config.version
            )));
        }
        let validation_errors = Self::validate_config(&config);
        if !validation_errors.is_empty() {
            return Err(TemplateConfigError::ValidationErrors(validation_errors));
        }
        info!(
            "Loaded {} templates from external file",
            config.templates.len()
        );

        let mut write_guard = self.config.write().await;
        *write_guard = config;
        Ok(())
    }

    /// Reload templates from the config file
    pub async fn reload(&self) -> Result<(), TemplateConfigError> {
        self.load().await
    }

    /// Validate a single template and return any errors
    pub fn validate_template(template: &ProjectTemplate) -> Vec<String> {
        let mut errors = Vec::new();

        // Check for empty slug
        if template.slug.is_empty() {
            errors.push("Slug cannot be empty".to_string());
        } else if template.slug.chars().count() > MAX_TEMPLATE_SLUG_CHARS {
            errors.push(format!(
                "Slug cannot exceed {MAX_TEMPLATE_SLUG_CHARS} characters"
            ));
        }

        // Check for empty name
        if template.name.is_empty() {
            errors.push("Name cannot be empty".to_string());
        }

        if template.kind == TemplateKind::Service {
            if template.version.trim().is_empty() {
                errors.push("Service template version cannot be empty".to_string());
            } else if let Err(error) = semver::Version::parse(&template.version) {
                errors.push(format!(
                    "Service template version '{}' is not valid Semantic Versioning: {error}",
                    template.version
                ));
            }
            if template.preset != "dockerfile" {
                errors.push(
                    "Service templates must use the dockerfile preset until native multi-container workloads are supported"
                        .to_string(),
                );
            }
            if template
                .image
                .as_deref()
                .is_none_or(|image| image.trim().is_empty())
            {
                errors.push("Service templates must declare a container image".to_string());
            } else if template
                .image
                .as_deref()
                .is_some_and(|image| !is_pinned_image_reference(image))
            {
                errors.push(
                    "Service templates must use an immutable sha256 image digest".to_string(),
                );
            }
            if template
                .exposed_port
                .is_none_or(|port| !(1..=65_535).contains(&port))
            {
                errors.push(
                    "Service templates must declare an exposed port between 1 and 65535"
                        .to_string(),
                );
            }
        }

        let mut environment_variable_names = std::collections::BTreeSet::new();
        for variable in &template.env_vars {
            if variable.name.trim().is_empty() {
                errors.push("Environment variable name cannot be empty".to_string());
            } else if !environment_variable_names.insert(variable.name.as_str()) {
                errors.push(format!(
                    "Environment variable '{}' is declared more than once",
                    variable.name
                ));
            }
            if variable.is_secret() && variable.default.is_some() {
                errors.push(format!(
                    "Secret environment variable '{}' cannot declare a literal default; use a secure generator or require user input",
                    variable.name
                ));
            }
        }

        // Check for valid git URL
        if template.git.url.is_empty() {
            errors.push("Git URL cannot be empty".to_string());
        } else if !template.git.url.starts_with("http://")
            && !template.git.url.starts_with("https://")
            && !template.git.url.starts_with("git@")
        {
            errors.push(format!("Invalid git URL: {}", template.git.url));
        }

        // Validate services against known list
        for service in &template.services {
            let service_lower = service.to_lowercase();
            if !VALID_SERVICES.contains(&service_lower.as_str()) {
                errors.push(format!(
                    "Unknown service '{}'. Valid services are: {}",
                    service,
                    VALID_SERVICES.join(", ")
                ));
            }
        }

        for (service, bindings) in &template.managed_service_bindings {
            if !template
                .services
                .iter()
                .any(|required| required.eq_ignore_ascii_case(service))
            {
                errors.push(format!(
                    "Managed service bindings for '{service}' require it to be listed in services"
                ));
            }
            for (target, source) in bindings {
                if target.trim().is_empty() || source.trim().is_empty() {
                    errors.push(format!(
                        "Managed service binding names for '{service}' cannot be empty"
                    ));
                }
            }
        }

        if let Some(command) = &template.command {
            if command.is_empty() {
                errors.push("Command must contain only non-empty arguments".to_string());
            }
            if command.len() > 64 {
                errors.push("Command cannot contain more than 64 arguments".to_string());
            }
            if command.iter().any(|part| {
                part.trim().is_empty() || part.len() > 1_024 || part.chars().any(char::is_control)
            }) {
                errors.push(
                    "Command arguments must be non-empty, at most 1024 bytes, and contain no control characters"
                        .to_string(),
                );
            }
        }

        if let Some(path) = template.health_check_path.as_deref() {
            if path.len() > 2_048
                || !path.starts_with('/')
                || path.contains('@')
                || path.contains("://")
                || path.chars().any(char::is_control)
            {
                errors.push(
                    "Health-check path must be a safe relative HTTP path starting with '/'"
                        .to_string(),
                );
            }
        }

        if let Some(resources) = &template.resources {
            for (name, value) in [
                ("cpu_request", resources.cpu_request),
                ("cpu_limit", resources.cpu_limit),
                ("memory_request", resources.memory_request),
                ("memory_limit", resources.memory_limit),
            ] {
                if value.is_some_and(|value| value <= 0) {
                    errors.push(format!("Template resource {name} must be positive"));
                }
            }
            if resources
                .cpu_request
                .zip(resources.cpu_limit)
                .is_some_and(|(request, limit)| request > limit)
            {
                errors.push("Template cpu_request cannot exceed cpu_limit".to_string());
            }
            if resources
                .memory_request
                .zip(resources.memory_limit)
                .is_some_and(|(request, limit)| request > limit)
            {
                errors.push("Template memory_request cannot exceed memory_limit".to_string());
            }
        }

        // Check for empty preset
        if template.preset.is_empty() {
            errors.push("Preset cannot be empty".to_string());
        } else if !VALID_PRESETS.contains(&template.preset.as_str()) {
            errors.push(format!(
                "Unknown preset '{}'. Valid presets are: {}",
                template.preset,
                VALID_PRESETS.join(", ")
            ));
        }

        errors
    }

    /// Validate all templates in a config and return validation errors
    pub fn validate_config(config: &TemplatesConfig) -> Vec<TemplateValidationError> {
        let mut validation_errors = Vec::new();
        let mut slugs = BTreeSet::new();

        for template in &config.templates {
            let mut errors = Self::validate_template(template);
            if !slugs.insert(template.slug.as_str()) {
                errors.push(format!(
                    "Template slug '{}' is declared more than once",
                    template.slug
                ));
            }
            if !errors.is_empty() {
                validation_errors.push(TemplateValidationError {
                    slug: template.slug.clone(),
                    errors,
                });
            }
        }

        validation_errors
    }

    /// Load and merge additional templates from a file
    /// Returns validation errors if any templates are invalid
    pub async fn load_additional(&self, path: &std::path::Path) -> Result<(), TemplateConfigError> {
        if !path.exists() {
            return Err(TemplateConfigError::IoError(format!(
                "Additional templates file not found: {:?}",
                path
            )));
        }

        info!("Loading additional templates from {:?}", path);
        let additional_config = TemplatesConfig::from_file(path)?;
        let contains_service_templates = additional_config
            .templates
            .iter()
            .any(|template| template.kind == TemplateKind::Service);
        if contains_service_templates
            && additional_config.version != SERVICE_TEMPLATE_SCHEMA_VERSION
        {
            return Err(TemplateConfigError::ParseError(format!(
                "Additional service templates require catalog schema '{}'; received '{}'",
                SERVICE_TEMPLATE_SCHEMA_VERSION, additional_config.version
            )));
        }

        // Validate additional templates
        let validation_errors = Self::validate_config(&additional_config);
        if !validation_errors.is_empty() {
            for err in &validation_errors {
                warn!("Template validation error: {}", err);
            }
            return Err(TemplateConfigError::ValidationErrors(validation_errors));
        }

        // Merge with existing templates
        let mut write_guard = self.config.write().await;
        for template in additional_config.templates {
            // Check for duplicate slugs
            if write_guard.get_by_slug(&template.slug).is_some() {
                warn!(
                    "Template with slug '{}' already exists, skipping",
                    template.slug
                );
                continue;
            }
            info!("Adding template: {} ({})", template.name, template.slug);
            write_guard.templates.push(template);
        }

        info!(
            "Total templates after merge: {}",
            write_guard.templates.len()
        );
        Ok(())
    }

    /// Get all public templates
    pub async fn list_templates(&self) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config.public_templates().into_iter().cloned().collect();
        templates.sort_by_key(|a| a.sort_order);
        templates
    }

    /// Get a template by slug
    pub async fn get_template(&self, slug: &str) -> Result<ProjectTemplate, TemplateConfigError> {
        let config = self.config.read().await;
        config
            .get_by_slug(slug)
            .filter(|template| template.is_public)
            .cloned()
            .ok_or_else(|| TemplateConfigError::NotFound(slug.to_string()))
    }

    /// Resolve a service release together with the catalog schema that
    /// interpreted it. This is the only shape persisted on service projects.
    pub async fn get_service_template_instance(
        &self,
        slug: &str,
    ) -> Result<ServiceTemplateInstance, TemplateConfigError> {
        let config = self.config.read().await;
        if config.version != SERVICE_TEMPLATE_SCHEMA_VERSION {
            return Err(TemplateConfigError::ParseError(format!(
                "Unsupported template catalog schema '{}'; expected '{}'",
                config.version, SERVICE_TEMPLATE_SCHEMA_VERSION
            )));
        }
        let template = config
            .get_by_slug(slug)
            .filter(|template| template.is_public && template.kind == TemplateKind::Service)
            .cloned()
            .ok_or_else(|| TemplateConfigError::NotFound(slug.to_string()))?;
        let instance = ServiceTemplateInstance::new(config.version.clone(), template);
        instance
            .validate()
            .map_err(|error| TemplateConfigError::ParseError(error.to_string()))?;
        Ok(instance)
    }

    /// Get all available tags
    pub async fn list_tags(&self) -> Vec<String> {
        let config = self.config.read().await;
        config.all_tags()
    }

    /// Set configuration directly (useful for testing)
    pub async fn set_config(&self, config: TemplatesConfig) {
        let mut write_guard = self.config.write().await;
        *write_guard = config;
    }
}

impl Clone for TemplateService {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            config_path: self.config_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
version: "1"
templates:
  - slug: nextjs-saas-starter
    name: Next.js SaaS Starter
    description: A complete SaaS starter kit with authentication, billing, and more
    image_url: https://example.com/nextjs-saas.png
    git:
      url: https://github.com/temps-templates/nextjs-saas-starter.git
      ref: main
    preset: nextjs
    tags:
      - saas
      - nextjs
      - typescript
    features:
      - Authentication with NextAuth.js
      - Stripe subscription billing
      - PostgreSQL database
      - Tailwind CSS styling
    services:
      - postgres
      - redis
    env_vars:
      - name: NEXTAUTH_SECRET
        description: Secret for NextAuth.js sessions
        required: true
      - name: STRIPE_SECRET_KEY
        description: Stripe secret API key
        example: sk_test_...
        required: true
      - name: STRIPE_WEBHOOK_SECRET
        description: Stripe webhook signing secret
        example: whsec_...
        required: true
    is_public: true
    is_featured: true
    sort_order: 1

  - slug: fastapi-backend
    name: FastAPI Backend
    description: Production-ready FastAPI backend with PostgreSQL
    git:
      url: https://gitlab.com/temps-templates/fastapi-backend.git
      ref: main
    preset: fastapi
    tags:
      - backend
      - python
      - api
    features:
      - Async PostgreSQL with SQLAlchemy
      - JWT authentication
      - OpenAPI documentation
    services:
      - postgres
    env_vars:
      - name: SECRET_KEY
        description: Application secret key
        required: true
    is_public: true
    is_featured: false
    sort_order: 10
"#;

    #[test]
    fn test_parse_templates_config() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();

        assert_eq!(config.version, "1");
        assert_eq!(config.templates.len(), 2);

        let first = &config.templates[0];
        assert_eq!(first.slug, "nextjs-saas-starter");
        assert_eq!(first.name, "Next.js SaaS Starter");
        assert_eq!(first.preset, "nextjs");
        assert!(first.is_public);
        assert!(first.is_featured);
        assert_eq!(first.sort_order, 1);

        // Check Git ref
        assert_eq!(
            first.git.url,
            "https://github.com/temps-templates/nextjs-saas-starter.git"
        );
        assert_eq!(first.git.r#ref, "main");

        // Check services (simple string list)
        assert_eq!(first.services.len(), 2);
        assert_eq!(first.services[0], "postgres");
        assert_eq!(first.services[1], "redis");

        // Check env vars
        assert_eq!(first.env_vars.len(), 3);
        assert_eq!(first.env_vars[0].name, "NEXTAUTH_SECRET");
        assert!(first.env_vars[0].required);

        // Check second template uses GitLab
        let second = &config.templates[1];
        assert_eq!(
            second.git.url,
            "https://gitlab.com/temps-templates/fastapi-backend.git"
        );
    }

    #[test]
    fn test_public_templates() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();
        let public = config.public_templates();
        assert_eq!(public.len(), 2);
    }

    #[test]
    fn managed_service_compatibility_uses_one_canonical_family() {
        assert!(managed_service_types_compatible("postgresql", "postgres"));
        assert!(managed_service_types_compatible("mysql", "MARIADB"));
        assert!(managed_service_types_compatible("s3", "rustfs"));
        assert!(managed_service_types_compatible("object-storage", "S3"));
        assert!(!managed_service_types_compatible("redis", "postgres"));
    }

    #[test]
    fn test_get_by_slug() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();

        let template = config.get_by_slug("nextjs-saas-starter");
        assert!(template.is_some());
        assert_eq!(template.unwrap().name, "Next.js SaaS Starter");

        let not_found = config.get_by_slug("nonexistent");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_all_tags() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();
        let tags = config.all_tags();

        assert!(tags.contains(&"saas".to_string()));
        assert!(tags.contains(&"python".to_string()));
        assert!(tags.contains(&"backend".to_string()));
    }

    #[test]
    fn test_empty_config() {
        let yaml = "";
        let config = TemplatesConfig::from_yaml(yaml).unwrap();
        assert!(config.templates.is_empty());
        assert_eq!(config.version, "1");
    }

    #[test]
    fn test_minimal_template() {
        let yaml = r#"
templates:
  - slug: minimal
    name: Minimal Template
    git:
      url: https://github.com/test/minimal.git
    preset: dockerfile
"#;
        let config = TemplatesConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.templates.len(), 1);

        let t = &config.templates[0];
        assert_eq!(t.slug, "minimal");
        assert_eq!(t.kind, TemplateKind::Starter);
        assert!(t.is_public); // default
        assert!(!t.is_featured); // default
        assert_eq!(t.sort_order, 0); // default
        assert!(t.tags.is_empty());
        assert!(t.services.is_empty());
        assert!(t.env_vars.is_empty());
        assert_eq!(t.git.r#ref, "main"); // default ref
    }

    #[test]
    fn bundled_keycloak_is_a_pinned_postgres_backed_service() {
        let template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");

        assert_eq!(template.kind, TemplateKind::Service);
        assert_eq!(
            template.image_url.as_deref(),
            Some("/templates/keycloak.svg")
        );
        assert_eq!(
            template.image.as_deref(),
            Some("quay.io/keycloak/keycloak@sha256:9d1f1b2b7261ff53c66cb1092dfcdc34a5fb77e81f9e6a6e75b8b6a795de8067")
        );
        assert_eq!(template.command, Some(vec!["start".to_string()]));
        assert_eq!(
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_limit),
            Some(1_000_000)
        );
        assert_eq!(
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_limit),
            Some(1536)
        );
        assert_eq!(template.services, vec!["postgres".to_string()]);
        assert_eq!(
            template
                .managed_service_bindings
                .get("postgres")
                .and_then(|bindings| bindings.get("KC_DB_USERNAME"))
                .map(String::as_str),
            Some("POSTGRES_USER")
        );
        assert!(TemplateService::validate_template(&template).is_empty());
    }

    #[test]
    fn bundled_browserless_is_a_pinned_authenticated_service() {
        let template = bundled_template_by_slug("browserless")
            .expect("Browserless should be part of the reviewed catalog");

        assert_eq!(template.kind, TemplateKind::Service);
        assert_eq!(
            template.image_url.as_deref(),
            Some("/templates/browserless.svg")
        );
        assert_eq!(
            template.image.as_deref(),
            Some("ghcr.io/browserless/chromium@sha256:6f3149efabf5e04a44385974448c85264398db416bc63f884564316d0fcadc3e")
        );
        assert_eq!(template.exposed_port, Some(3000));
        assert_eq!(template.health_check_path.as_deref(), Some("/docs"));
        assert!(template.services.is_empty());

        let token = template
            .env_vars
            .iter()
            .find(|variable| variable.name == "TOKEN")
            .expect("Browserless must require an access token");
        assert!(token.required);
        assert_eq!(token.default_generator.as_deref(), Some("random_secret"));

        let external = template
            .env_vars
            .iter()
            .find(|variable| variable.name == "EXTERNAL")
            .expect("Browserless must know its proxy-facing URL");
        assert!(external.required);
        assert_eq!(external.default_generator.as_deref(), Some("app_url"));
        assert!(TemplateService::validate_template(&template).is_empty());
    }

    #[test]
    fn service_template_rejects_a_mutable_version_tag() {
        let mut template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        template.image = Some("quay.io/keycloak/keycloak:26.7.2".to_string());

        let errors = TemplateService::validate_template(&template);

        assert!(
            errors
                .iter()
                .any(|error| error == "Service templates must use an immutable sha256 image digest"),
            "unexpected validation errors: {errors:?}"
        );
    }

    #[test]
    fn service_template_rejects_invalid_runtime_contract() {
        let mut template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        template.image = Some(format!("@sha256:{}", "a".repeat(64)));
        template.command = Some(vec!["ok".to_string(); 65]);
        template.health_check_path = Some("https://attacker.test/ready".to_string());

        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error.contains("immutable sha256 image digest")));
        assert!(errors
            .iter()
            .any(|error| error.contains("more than 64 arguments")));
        assert!(errors
            .iter()
            .any(|error| error.contains("Health-check path")));
    }

    #[test]
    fn template_resources_reject_requests_above_limits() {
        let mut template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        let resources = template
            .resources
            .as_mut()
            .expect("Keycloak should define a runtime profile");
        resources.cpu_request = Some(2_000_000);
        resources.cpu_limit = Some(1_000_000);

        let errors = TemplateService::validate_template(&template);

        assert!(
            errors
                .iter()
                .any(|error| error == "Template cpu_request cannot exceed cpu_limit"),
            "unexpected validation errors: {errors:?}"
        );
    }

    #[test]
    fn test_serialize_config() {
        let config = TemplatesConfig {
            version: "1".to_string(),
            templates: vec![ProjectTemplate {
                slug: "test".to_string(),
                name: "Test Template".to_string(),
                version: "1.0.0".to_string(),
                kind: TemplateKind::Starter,
                description: Some("A test template".to_string()),
                image_url: None,
                screenshot_url: None,
                git: GitRef {
                    url: "https://github.com/test/test-repo.git".to_string(),
                    path: None,
                    r#ref: "main".to_string(),
                },
                preset: "nextjs".to_string(),
                preset_config: None,
                image: None,
                command: None,
                resources: None,
                exposed_port: None,
                health_check_path: None,
                tags: vec!["test".to_string()],
                features: vec!["Feature 1".to_string()],
                services: vec!["postgres".to_string()],
                managed_service_bindings: BTreeMap::new(),
                env_vars: vec![],
                is_public: true,
                is_featured: false,
                sort_order: 0,
            }],
        };

        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("slug: test"));
        assert!(yaml.contains("name: Test Template"));
        assert!(yaml.contains("https://github.com/test/test-repo.git"));
    }

    #[tokio::test]
    async fn test_template_service() {
        let service = TemplateService::new(None).expect("bundled templates must load");

        // Set config directly for testing
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();
        service.set_config(config).await;

        // Test list_templates
        let templates = service.list_templates().await;
        assert_eq!(templates.len(), 2);
        // Should be sorted by sort_order
        assert_eq!(templates[0].slug, "nextjs-saas-starter");

        // Test get_template
        let template = service.get_template("fastapi-backend").await.unwrap();
        assert_eq!(template.name, "FastAPI Backend");

        // Test not found
        let err = service.get_template("nonexistent").await;
        assert!(err.is_err());

        // Test list_tags
        let tags = service.list_tags().await;
        assert!(!tags.is_empty());
    }

    #[tokio::test]
    async fn private_templates_are_not_available_through_public_lookups() {
        let service = TemplateService::new(None).expect("bundled templates must load");
        let mut template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        template.is_public = false;
        service
            .set_config(TemplatesConfig {
                version: SERVICE_TEMPLATE_SCHEMA_VERSION.to_string(),
                templates: vec![template],
            })
            .await;

        assert!(matches!(
            service.get_template("keycloak").await,
            Err(TemplateConfigError::NotFound(_))
        ));
        assert!(matches!(
            service.get_service_template_instance("keycloak").await,
            Err(TemplateConfigError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn external_version_one_starter_catalogs_remain_supported() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().expect("temporary template catalog");
        file.write_all(SAMPLE_CONFIG.as_bytes())
            .expect("write version-one catalog");
        let service = TemplateService::new(Some(file.path().to_path_buf()))
            .expect("bundled templates must load");

        service
            .load()
            .await
            .expect("existing starter-only catalogs must remain loadable");
        assert_eq!(service.list_templates().await.len(), 2);
    }

    #[tokio::test]
    async fn external_version_one_service_catalogs_are_rejected() {
        use std::io::Write;

        let template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        let yaml = TemplatesConfig {
            version: "1".to_string(),
            templates: vec![template],
        }
        .to_yaml()
        .expect("serialize incompatible service catalog");
        let mut file = tempfile::NamedTempFile::new().expect("temporary template catalog");
        file.write_all(yaml.as_bytes())
            .expect("write incompatible service catalog");
        let service = TemplateService::new(Some(file.path().to_path_buf()))
            .expect("bundled templates must load");

        let error = service
            .load()
            .await
            .expect_err("schema-one service definitions must not load");
        assert!(matches!(error, TemplateConfigError::ParseError(_)));
    }

    #[tokio::test]
    async fn additional_service_catalogs_cannot_be_relabelled_as_schema_two() {
        use std::io::Write;

        let template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");
        let yaml = TemplatesConfig {
            version: "1".to_string(),
            templates: vec![template],
        }
        .to_yaml()
        .expect("serialize incompatible service catalog");
        let mut file = tempfile::NamedTempFile::new().expect("temporary template catalog");
        file.write_all(yaml.as_bytes())
            .expect("write incompatible service catalog");
        let service = TemplateService::new(None).expect("bundled templates must load");

        let error = service
            .load_additional(file.path())
            .await
            .expect_err("schema-one service definitions must not be persisted as schema two");
        assert!(matches!(error, TemplateConfigError::ParseError(_)));
    }

    #[test]
    fn test_bundled_templates_parse_and_validate() {
        // Every YAML file in the bundled template directory is embedded at
        // compile time. Guard against malformed files, duplicate slugs, folder
        // mismatches, invalid git URLs, and unknown managed services.
        let config = bundled_templates_config().expect("bundled templates must parse");
        assert!(
            !config.templates.is_empty(),
            "bundled templates should not be empty"
        );

        let errors = TemplateService::validate_config(config);
        assert!(
            errors.is_empty(),
            "bundled templates have validation errors: {:?}",
            errors
        );

        // The Observability Starter is the one-click activation demo surfaced in
        // the empty projects state — it must stay featured and bring a database.
        let starter = config
            .get_by_slug("observability-starter")
            .expect("observability-starter template must exist");
        assert!(
            starter.is_featured,
            "observability-starter must be featured"
        );
        assert!(
            starter.services.iter().any(|s| s == "postgres"),
            "observability-starter must depend on postgres"
        );
        assert!(
            starter.env_vars.is_empty(),
            "observability-starter must not ask for platform-managed observability variables"
        );
    }

    #[test]
    fn bundled_template_files_are_sorted_deterministically() {
        let alpha = r#"
slug: alpha
name: Alpha
kind: starter
git:
  url: https://example.com/alpha.git
preset: nextjs
sort_order: 10
"#;
        let zulu = r#"
slug: zulu
name: Zulu
kind: service
version: 1.0.0
git:
  url: https://example.com/zulu.git
preset: dockerfile
image: example.com/zulu@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
exposed_port: 3000
sort_order: 0
"#;

        let config = parse_bundled_templates(vec![
            ("services/zulu.yaml", zulu),
            ("starters/alpha.yaml", alpha),
        ])
        .expect("valid template files should load");

        assert_eq!(
            config
                .templates
                .iter()
                .map(|template| template.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["zulu", "alpha"]
        );
    }

    #[test]
    fn bundled_template_folder_must_match_declared_kind() {
        let yaml = r#"
slug: misplaced
name: Misplaced
kind: service
version: 1.0.0
git:
  url: https://example.com/misplaced.git
preset: dockerfile
image: example.com/misplaced@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
exposed_port: 3000
"#;

        let error = parse_bundled_templates(vec![("starters/misplaced.yaml", yaml)])
            .expect_err("a service in the starter directory must be rejected");

        assert!(
            matches!(error, TemplateConfigError::ParseError(message) if message.contains("declares kind"))
        );
    }

    #[test]
    fn bundled_template_filename_must_match_slug() {
        let yaml = r#"
slug: actual-slug
name: Actual slug
kind: starter
git:
  url: https://example.com/actual.git
preset: nextjs
"#;

        let error = parse_bundled_templates(vec![("starters/wrong-name.yaml", yaml)])
            .expect_err("a mismatched filename must be rejected");

        assert!(
            matches!(error, TemplateConfigError::ParseError(message) if message.contains("must use its slug"))
        );
    }

    #[test]
    fn bundled_template_slugs_must_be_unique_across_subdirectories() {
        let yaml = r#"
slug: duplicate
name: Duplicate
kind: starter
git:
  url: https://example.com/duplicate.git
preset: nextjs
"#;

        let error = parse_bundled_templates(vec![
            ("starters/product/duplicate.yaml", yaml),
            ("starters/framework/duplicate.yaml", yaml),
        ])
        .expect_err("duplicate slugs must be rejected across the whole catalog");

        assert!(
            matches!(error, TemplateConfigError::ParseError(message) if message.contains("duplicated"))
        );
    }

    #[test]
    fn template_config_rejects_duplicate_slugs() {
        let mut config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).expect("sample config parses");
        config.templates.push(config.templates[0].clone());

        let errors = TemplateService::validate_config(&config);

        assert!(errors.iter().any(|error| {
            error.slug == config.templates[0].slug
                && error
                    .errors
                    .iter()
                    .any(|message| message.contains("declared more than once"))
        }));
    }

    #[test]
    fn telemetry_slug_allowlist_exactly_matches_bundled_catalog() {
        let config = bundled_templates_config().expect("bundled templates must parse");
        let bundled: std::collections::BTreeSet<&str> = config
            .templates
            .iter()
            .filter(|template| template.is_public)
            .map(|template| template.slug.as_str())
            .collect();
        let allowlisted: std::collections::BTreeSet<&str> = bundled
            .iter()
            .copied()
            .filter(|slug| telemetry_safe_template_slug(slug).is_some())
            .collect();

        assert_eq!(
            allowlisted, bundled,
            "every bundled slug must be explicitly reviewed for telemetry, and custom slugs must stay excluded"
        );
        assert_eq!(
            telemetry_safe_template_slug("observability-starter"),
            Some("observability-starter")
        );
        assert_eq!(telemetry_safe_template_slug("customer-acme-private"), None);
    }

    #[test]
    fn external_override_reusing_bundled_slug_is_custom_provenance() {
        let mut config = bundled_templates_config()
            .expect("bundled templates must parse")
            .clone();
        let template = config
            .templates
            .iter_mut()
            .find(|template| template.slug == "observability-starter")
            .expect("observability template must be bundled");
        template.git.url = "https://example.com/operator/observability.git".to_string();

        assert_eq!(bundled_telemetry_template_slug(template), None);
        assert_eq!(template_provenance(template), CUSTOM_TEMPLATE_PROVENANCE);
        assert_eq!(
            telemetry_safe_template_slug(template_provenance(template)),
            None
        );
    }

    #[test]
    fn template_validation_rejects_slug_longer_than_project_column() {
        let mut config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).expect("sample config parses");
        config.templates[0].slug = "x".repeat(MAX_TEMPLATE_SLUG_CHARS + 1);

        let errors = TemplateService::validate_config(&config);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].errors.iter().any(|error| error.contains("255")));
    }

    #[test]
    fn service_template_release_is_complete_and_uses_catalog_schema() {
        let instance = ServiceTemplateInstance::new(
            SERVICE_TEMPLATE_SCHEMA_VERSION,
            bundled_template_by_slug("keycloak").expect("keycloak service release must be bundled"),
        );

        assert_eq!(instance.slug, "keycloak");
        assert_eq!(instance.version, "1.0.0");
        assert_eq!(instance.schema_version, "2");
        assert_eq!(instance.template.kind, TemplateKind::Service);
        assert!(instance.template.image.is_some());
    }

    #[test]
    fn service_template_instance_rejects_an_invalid_resolved_definition() {
        let mut template = bundled_template_by_slug("browserless")
            .expect("Browserless should be part of the reviewed catalog");
        template.image = Some("ghcr.io/browserless/chromium:v2.56.0".to_string());
        let instance = ServiceTemplateInstance::new(SERVICE_TEMPLATE_SCHEMA_VERSION, template);

        assert!(matches!(
            instance.validate(),
            Err(ServiceTemplateInstanceError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn service_template_release_comparison_requires_same_family_and_newer_version() {
        let applied = ServiceTemplateInstance::new(
            SERVICE_TEMPLATE_SCHEMA_VERSION,
            bundled_template_by_slug("keycloak").expect("keycloak service release must be bundled"),
        );
        let mut newer = applied.clone();
        newer.version = "1.1.0".to_string();
        newer.template.version = newer.version.clone();

        assert!(newer.is_newer_than(&applied).unwrap());
        assert!(!applied.is_newer_than(&newer).unwrap());

        let mut other_family = newer;
        other_family.slug = "browserless".to_string();
        assert!(!other_family.is_newer_than(&applied).unwrap());
    }

    #[test]
    fn service_template_validation_rejects_mutable_or_ambiguous_inputs() {
        let mut template = bundled_template_by_slug("keycloak")
            .expect("keycloak service template must be bundled");
        template.version.clear();
        template.env_vars.push(template.env_vars[0].clone());

        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error == "Service template version cannot be empty"));
        assert!(errors
            .iter()
            .any(|error| error.contains("declared more than once")));

        template.version = "next".to_string();
        template.env_vars.pop();
        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error.contains("not valid Semantic Versioning")));

        template.version = "1.0.0".to_string();
        template.image = Some("example.test/keycloak:latest".to_string());
        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error.contains("immutable sha256 image digest")));
    }

    #[test]
    fn service_input_secret_policy_distinguishes_credentials_from_configuration() {
        let template = bundled_template_by_slug("keycloak")
            .expect("keycloak service template must be bundled");
        let password = template
            .env_vars
            .iter()
            .find(|variable| variable.name == "KC_BOOTSTRAP_ADMIN_PASSWORD")
            .expect("password input must exist");
        let username = template
            .env_vars
            .iter()
            .find(|variable| variable.name == "KC_BOOTSTRAP_ADMIN_USERNAME")
            .expect("username input must exist");

        assert!(password.is_secret());
        assert!(!username.is_secret());
    }

    #[test]
    fn service_template_validation_rejects_literal_secret_defaults() {
        let mut template = bundled_template_by_slug("keycloak")
            .expect("keycloak service template must be bundled");
        let password = template
            .env_vars
            .iter_mut()
            .find(|variable| variable.name == "KC_BOOTSTRAP_ADMIN_PASSWORD")
            .expect("password input must exist");
        password.default_generator = None;
        password.default = Some("do-not-publish-me".to_string());

        let errors = TemplateService::validate_template(&template);
        assert!(errors.iter().any(|error| {
            error.contains("KC_BOOTSTRAP_ADMIN_PASSWORD")
                && error.contains("cannot declare a literal default")
        }));
    }

    #[test]
    fn test_various_git_providers() {
        let yaml = r#"
templates:
  - slug: github-template
    name: GitHub Template
    git:
      url: https://github.com/owner/repo.git
    preset: nextjs

  - slug: gitlab-template
    name: GitLab Template
    git:
      url: https://gitlab.com/owner/repo.git
      ref: develop
    preset: fastapi

  - slug: bitbucket-template
    name: Bitbucket Template
    git:
      url: https://bitbucket.org/owner/repo.git
      path: packages/app
      ref: v1.0.0
    preset: nodejs
"#;
        let config = TemplatesConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.templates.len(), 3);

        // GitHub
        assert!(config.templates[0].git.url.contains("github.com"));

        // GitLab with custom branch
        assert!(config.templates[1].git.url.contains("gitlab.com"));
        assert_eq!(config.templates[1].git.r#ref, "develop");

        // Bitbucket with path (monorepo) and tag
        assert!(config.templates[2].git.url.contains("bitbucket.org"));
        assert_eq!(
            config.templates[2].git.path,
            Some("packages/app".to_string())
        );
        assert_eq!(config.templates[2].git.r#ref, "v1.0.0");
    }
}
