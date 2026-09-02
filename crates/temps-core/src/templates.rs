// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project Templates Configuration
//!
//! Curated project templates that users can use to quickly create new projects.
//! Templates are defined in a YAML configuration file for easy customization.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
    /// Frontend-side generator for the default value. Recognised values:
    /// `app_url` (https://{repo}.{base_domain}), `random_secret` (32-byte base64),
    /// `random_hex_32` (32-byte hex). Unknown values are ignored client-side.
    #[serde(default)]
    pub default_generator: Option<String>,
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

    /// Get featured templates
    pub fn featured_templates(&self) -> Vec<&ProjectTemplate> {
        self.templates
            .iter()
            .filter(|t| t.is_public && t.is_featured)
            .collect()
    }

    /// Get templates by tag
    pub fn templates_by_tag(&self, tag: &str) -> Vec<&ProjectTemplate> {
        self.templates
            .iter()
            .filter(|t| t.is_public && t.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)))
            .collect()
    }

    /// Get a template by slug
    pub fn get_by_slug(&self, slug: &str) -> Option<&ProjectTemplate> {
        self.templates.iter().find(|t| t.slug == slug)
    }

    /// Get public templates for one gallery.
    pub fn templates_by_kind(&self, kind: TemplateKind) -> Vec<&ProjectTemplate> {
        self.templates
            .iter()
            .filter(|template| template.is_public && template.kind == kind)
            .collect()
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
    "postgres",
    "mysql",
    "mariadb",
    "redis",
    "mongodb",
    "minio",
    "rabbitmq",
    "memcached",
    "clickhouse",
    "influxdb",
    "cassandra",
    "neo4j",
    "opensearch",
    "valkey",
];

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

/// Bundled default templates (embedded at compile time)
const BUNDLED_TEMPLATES: &str = include_str!("../templates.yaml");

/// Maximum template slug length accepted by the catalog and project schema.
pub const MAX_TEMPLATE_SLUG_CHARS: usize = 255;

/// Prefix used for server-attested service-catalog provenance stored on a
/// project. The suffix is a public catalog slug that was matched against the
/// catalog and install-plan digest by the project-creation handler.
pub const SERVICE_CATALOG_TEMPLATE_PREFIX: &str = "service_catalog:";

/// Internal marker held only between project creation and the first saved
/// Compose revision. It binds server-validated catalog metadata to the exact
/// normalized Compose bytes before telemetry attribution is promoted.
const PENDING_SERVICE_CATALOG_TEMPLATE_PREFIX: &str = "service_catalog_pending:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingServiceCatalogResolution {
    /// The stored value is not a pending service-catalog marker.
    NotPending,
    /// The first saved Compose source matches the server-attested template.
    Matched(String),
    /// The marker is malformed or the saved source does not match.
    Mismatched,
}

/// Fixed, reviewable labels that are safe to include in anonymous telemetry.
///
/// Operators can load private templates with arbitrary slugs. Those slugs must
/// never leave the instance, so telemetry callers must pass them through
/// [`telemetry_safe_template_slug`] and treat `None` as a custom template.
const BUNDLED_TELEMETRY_TEMPLATE_SLUGS: &[&str] = &[
    "observability-starter",
    "nextjs-saas-starter",
    "nextjs-docs-template",
    "keycloak",
];

/// Return a bundled template only when the slug is part of the embedded,
/// reviewed catalog. Runtime configuration files cannot influence this lookup.
pub fn bundled_template_by_slug(slug: &str) -> Option<ProjectTemplate> {
    TemplatesConfig::from_yaml(BUNDLED_TEMPLATES)
        .ok()?
        .templates
        .into_iter()
        .find(|template| template.slug == slug)
}

/// Stored provenance marker for projects created from an operator-defined
/// template. The operator's actual slug stays local to the template catalog.
pub const CUSTOM_TEMPLATE_PROVENANCE: &str = "custom";

/// Return the slug only when it is a fixed label from the bundled catalog.
pub fn telemetry_safe_template_slug(slug: &str) -> Option<&str> {
    BUNDLED_TELEMETRY_TEMPLATE_SLUGS
        .contains(&slug)
        .then_some(slug)
}

fn valid_service_catalog_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() + SERVICE_CATALOG_TEMPLATE_PREFIX.len() <= MAX_TEMPLATE_SLUG_CHARS
        && slug.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

/// Build bounded provenance for a service-catalog template after the caller
/// has verified the slug and install-plan digest against the live catalog.
///
/// This validates only the storage/wire shape. Callers must not use it as a
/// substitute for catalog attestation because arbitrary user text must never
/// be promoted into anonymous telemetry.
pub fn service_catalog_template_provenance(slug: &str) -> Option<String> {
    valid_service_catalog_slug(slug).then(|| format!("{SERVICE_CATALOG_TEMPLATE_PREFIX}{slug}"))
}

/// Build a server-only pending provenance marker bound to the exact Compose
/// source returned by the catalog detail endpoint.
pub fn pending_service_catalog_template_provenance(slug: &str, compose: &str) -> Option<String> {
    if !valid_service_catalog_slug(slug) {
        return None;
    }
    let checksum = hex::encode(Sha256::digest(compose.as_bytes()));
    let provenance = format!("{PENDING_SERVICE_CATALOG_TEMPLATE_PREFIX}{slug}:{checksum}");
    (provenance.len() <= MAX_TEMPLATE_SLUG_CHARS).then_some(provenance)
}

/// Resolve pending catalog provenance when the first immutable Compose source
/// is saved. A mismatch is terminal: callers should clear the pending marker
/// so unrelated content can never inherit a catalog template label later.
pub fn resolve_pending_service_catalog_template_provenance(
    provenance: &str,
    compose: &str,
) -> PendingServiceCatalogResolution {
    let Some(payload) = provenance.strip_prefix(PENDING_SERVICE_CATALOG_TEMPLATE_PREFIX) else {
        return PendingServiceCatalogResolution::NotPending;
    };
    let Some((slug, expected_checksum)) = payload.rsplit_once(':') else {
        return PendingServiceCatalogResolution::Mismatched;
    };
    if !valid_service_catalog_slug(slug)
        || expected_checksum.len() != 64
        || !expected_checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return PendingServiceCatalogResolution::Mismatched;
    }
    let actual_checksum = hex::encode(Sha256::digest(compose.as_bytes()));
    if actual_checksum != expected_checksum {
        return PendingServiceCatalogResolution::Mismatched;
    }
    service_catalog_template_provenance(slug)
        .map(PendingServiceCatalogResolution::Matched)
        .unwrap_or(PendingServiceCatalogResolution::Mismatched)
}

/// Recover the public service-catalog slug from previously attested project
/// provenance. Other provenance values, including operator templates, remain
/// private and return `None`.
pub fn telemetry_safe_service_catalog_slug(provenance: &str) -> Option<&str> {
    let slug = provenance.strip_prefix(SERVICE_CATALOG_TEMPLATE_PREFIX)?;
    valid_service_catalog_slug(slug).then_some(slug)
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
    let bundled = TemplatesConfig::from_yaml(BUNDLED_TEMPLATES).ok()?;
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
    pub fn new(config_path: Option<std::path::PathBuf>) -> Self {
        // Load bundled templates by default
        let config = match TemplatesConfig::from_yaml(BUNDLED_TEMPLATES) {
            Ok(config) => {
                info!("Loaded {} bundled templates", config.templates.len());
                config
            }
            Err(e) => {
                warn!("Failed to parse bundled templates: {}", e);
                TemplatesConfig::default()
            }
        };

        Self {
            config: Arc::new(RwLock::new(config)),
            config_path,
        }
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

        if template
            .command
            .as_ref()
            .is_some_and(|command| command.is_empty() || command.iter().any(|part| part.is_empty()))
        {
            errors.push("Command must contain only non-empty arguments".to_string());
        }

        if let Some(resources) = &template.resources {
            for (name, value) in [
                ("cpu_request", resources.cpu_request),
                ("memory_request", resources.memory_request),
                ("memory_limit", resources.memory_limit),
            ] {
                if value.is_some_and(|value| value <= 0) {
                    errors.push(format!("Template resource {name} must be positive"));
                }
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

        for template in &config.templates {
            let errors = Self::validate_template(template);
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

    /// Get featured templates
    pub async fn list_featured_templates(&self) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config.featured_templates().into_iter().cloned().collect();
        templates.sort_by_key(|a| a.sort_order);
        templates
    }

    /// Get templates filtered by tag
    pub async fn list_templates_by_tag(&self, tag: &str) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config.templates_by_tag(tag).into_iter().cloned().collect();
        templates.sort_by_key(|a| a.sort_order);
        templates
    }

    /// Get public templates for one gallery.
    pub async fn list_templates_by_kind(&self, kind: TemplateKind) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config
            .templates_by_kind(kind)
            .into_iter()
            .cloned()
            .collect();
        templates.sort_by_key(|template| template.sort_order);
        templates
    }

    /// Get a template by slug
    pub async fn get_template(&self, slug: &str) -> Result<ProjectTemplate, TemplateConfigError> {
        let config = self.config.read().await;
        config
            .get_by_slug(slug)
            .cloned()
            .ok_or_else(|| TemplateConfigError::NotFound(slug.to_string()))
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
    fn test_featured_templates() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();
        let featured = config.featured_templates();
        assert_eq!(featured.len(), 1);
        assert_eq!(featured[0].slug, "nextjs-saas-starter");
    }

    #[test]
    fn test_templates_by_tag() {
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();

        let saas = config.templates_by_tag("saas");
        assert_eq!(saas.len(), 1);
        assert_eq!(saas[0].slug, "nextjs-saas-starter");

        let python = config.templates_by_tag("python");
        assert_eq!(python.len(), 1);
        assert_eq!(python[0].slug, "fastapi-backend");

        // Case insensitive
        let backend = config.templates_by_tag("BACKEND");
        assert_eq!(backend.len(), 1);
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
            template.image.as_deref(),
            Some("quay.io/keycloak/keycloak:26.7.2")
        );
        assert_eq!(template.command, Some(vec!["start".to_string()]));
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
    fn test_serialize_config() {
        let config = TemplatesConfig {
            version: "1".to_string(),
            templates: vec![ProjectTemplate {
                slug: "test".to_string(),
                name: "Test Template".to_string(),
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
        let service = TemplateService::new(None);

        // Set config directly for testing
        let config = TemplatesConfig::from_yaml(SAMPLE_CONFIG).unwrap();
        service.set_config(config).await;

        // Test list_templates
        let templates = service.list_templates().await;
        assert_eq!(templates.len(), 2);
        // Should be sorted by sort_order
        assert_eq!(templates[0].slug, "nextjs-saas-starter");

        // Test list_featured_templates
        let featured = service.list_featured_templates().await;
        assert_eq!(featured.len(), 1);

        // Test get_template
        let template = service.get_template("fastapi-backend").await.unwrap();
        assert_eq!(template.name, "FastAPI Backend");

        // Test not found
        let err = service.get_template("nonexistent").await;
        assert!(err.is_err());

        // Test list_tags
        let tags = service.list_tags().await;
        assert!(!tags.is_empty());

        // Test list_templates_by_tag
        let python_templates = service.list_templates_by_tag("python").await;
        assert_eq!(python_templates.len(), 1);
    }

    #[test]
    fn test_bundled_templates_parse_and_validate() {
        // The bundled templates.yaml is embedded at compile time and loaded on
        // every startup. Guard against YAML breakage and invalid template
        // definitions (bad git URLs, unknown services, etc.).
        let config = TemplatesConfig::from_yaml(BUNDLED_TEMPLATES)
            .expect("bundled templates.yaml must parse");
        assert!(
            !config.templates.is_empty(),
            "bundled templates should not be empty"
        );

        let errors = TemplateService::validate_config(&config);
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
    fn telemetry_slug_allowlist_exactly_matches_bundled_catalog() {
        let config = TemplatesConfig::from_yaml(BUNDLED_TEMPLATES)
            .expect("bundled templates.yaml must parse");
        let bundled: std::collections::BTreeSet<&str> = config
            .templates
            .iter()
            .map(|template| template.slug.as_str())
            .collect();
        let allowlisted: std::collections::BTreeSet<&str> =
            BUNDLED_TELEMETRY_TEMPLATE_SLUGS.iter().copied().collect();

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
    fn service_catalog_provenance_is_bounded_and_round_trips_public_slugs() {
        let provenance = service_catalog_template_provenance("keycloak").unwrap();
        assert_eq!(provenance, "service_catalog:keycloak");
        assert_eq!(
            telemetry_safe_service_catalog_slug(&provenance),
            Some("keycloak")
        );

        let too_long = "x".repeat(MAX_TEMPLATE_SLUG_CHARS);
        for invalid in [
            "",
            "Private Service",
            "../private",
            "customer@example.com",
            too_long.as_str(),
        ] {
            assert!(service_catalog_template_provenance(invalid).is_none());
        }
        assert!(telemetry_safe_service_catalog_slug("custom").is_none());
    }

    #[test]
    fn pending_service_catalog_provenance_requires_the_exact_first_source() {
        let compose = "services:\n  keycloak:\n    image: quay.io/keycloak/keycloak:26.3.2\n";
        let pending = pending_service_catalog_template_provenance("keycloak", compose)
            .expect("public catalog slug should produce pending provenance");

        assert_eq!(
            resolve_pending_service_catalog_template_provenance(&pending, compose),
            PendingServiceCatalogResolution::Matched("service_catalog:keycloak".to_string())
        );
        assert_eq!(
            resolve_pending_service_catalog_template_provenance(
                &pending,
                "services:\n  unrelated:\n    image: example/unrelated:latest\n"
            ),
            PendingServiceCatalogResolution::Mismatched
        );
        assert_eq!(
            resolve_pending_service_catalog_template_provenance(
                "service_catalog:keycloak",
                compose
            ),
            PendingServiceCatalogResolution::NotPending
        );
        assert!(telemetry_safe_service_catalog_slug(&pending).is_none());
    }

    #[test]
    fn pending_service_catalog_provenance_rejects_malformed_or_oversized_values() {
        assert_eq!(
            resolve_pending_service_catalog_template_provenance(
                "service_catalog_pending:keycloak:not-a-sha256",
                "services: {}\n"
            ),
            PendingServiceCatalogResolution::Mismatched
        );
        let oversized_slug = "x".repeat(MAX_TEMPLATE_SLUG_CHARS);
        assert!(
            pending_service_catalog_template_provenance(&oversized_slug, "services: {}\n")
                .is_none()
        );
    }

    #[test]
    fn external_override_reusing_bundled_slug_is_custom_provenance() {
        let mut config = TemplatesConfig::from_yaml(BUNDLED_TEMPLATES)
            .expect("bundled templates.yaml must parse");
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
