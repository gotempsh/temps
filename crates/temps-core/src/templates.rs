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

/// Who owns a template's code once it is created (ADR-051).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    /// Source code the user takes over: forked or deployed from its public
    /// repository, never upgraded by the catalog afterwards.
    #[default]
    Starter,
    /// An upstream application pinned to a release. Its resolved definition is
    /// kept with every service it creates, and newer catalog releases are
    /// offered as upgrades. `service` is the kind's former name.
    #[serde(alias = "service")]
    App,
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

/// One deployable unit of a template: it becomes one service (a `projects`
/// row) when the template is created.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct TemplateServiceSpec {
    /// Name of the service, unique within the template. It names the created
    /// service and is how routes and variable references point at it.
    pub name: String,
    /// Source repository for this service when it differs from the template's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitRef>,
    /// Directory of the source this service builds from. Overrides the path
    /// of its repository reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Framework/preset to use (e.g., "nextjs", "python", "dockerfile")
    pub preset: String,
    /// Preset-specific configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_config: Option<serde_json::Value>,
    /// Prebuilt Docker image reference (e.g. "ghcr.io/org/app@sha256:…"). When
    /// set, the service pulls and runs this image instead of building from
    /// source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Optional command passed to the container image. This is needed for
    /// production images whose default command is intentionally a development
    /// mode (for example Keycloak).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Minimum/safe runtime resources for this service. These values are
    /// validated against operator-configured tenant ceilings before creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<TemplateResources>,
    /// Container port the service listens on. Falls back to the image's
    /// EXPOSE / 3000 default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposed_port: Option<i32>,
    /// HTTP health-check path probed after the container starts. Must start
    /// with '/'. Defaults to "/".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
    /// Variables of this service only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_vars: Vec<EnvVarTemplate>,
    /// Managed databases (types listed in the template's `databases`) linked
    /// to this service. Filled in on load: a service is linked to every
    /// database it binds, and a template's only service to all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<String>,
    /// Environment aliases populated from a linked managed database. The outer
    /// key is the database type and each inner entry maps an application
    /// variable to a variable supplied by that database.
    ///
    /// Example: `postgres.KC_DB_USERNAME: POSTGRES_USER`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub database_bindings: BTreeMap<String, BTreeMap<String, String>>,
}

impl TemplateServiceSpec {
    /// The repository this service builds from: its own, or the template's,
    /// with `directory` taking precedence over the reference's path.
    pub fn source(&self, template_git: &GitRef) -> GitRef {
        let mut git = self.git.clone().unwrap_or_else(|| template_git.clone());
        if let Some(directory) = self.directory.as_deref().filter(|d| !d.trim().is_empty()) {
            git.path = Some(directory.trim().to_string());
        }
        git
    }

    /// Whether the service runs a prebuilt image instead of building source.
    pub fn is_image(&self) -> bool {
        self.image
            .as_deref()
            .is_some_and(|image| !image.trim().is_empty())
    }
}

/// A path route between two services of the template (ADR-050).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TemplateRoute {
    /// Service whose hostnames receive the requests.
    pub source: String,
    /// Path prefix handed to `target`, such as `/api`.
    pub prefix: String,
    /// Service that answers requests under `prefix`.
    pub target: String,
    /// Forward `/api/users` as `/users`.
    #[serde(default)]
    pub strip_prefix: bool,
}

/// A curated template: one or more services, the managed databases they use,
/// the variables they share and the routes between them (ADR-051).
///
/// Two YAML shapes load into it. The current one lists service definitions
/// under `services`. The original one-service shape keeps the service's
/// fields at the top level and uses `services` for the managed databases; it
/// loads as a template whose only service is named after the slug. Which
/// shape a document uses is decided by its structure, never guessed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(try_from = "RawProjectTemplate")]
pub struct ProjectTemplate {
    /// Unique identifier for the template (used in URLs)
    pub slug: String,
    /// Display name
    pub name: String,
    /// Version of this template release. Apps pin this value and the
    /// complete resolved definition so catalog updates are always an
    /// explicit, reviewable upgrade rather than a silent runtime mutation.
    pub version: String,
    /// Who owns the code once the template is created.
    pub kind: TemplateKind,
    /// Short description
    pub description: Option<String>,
    /// URL to template image/icon
    pub image_url: Option<String>,
    /// URL to a full screenshot/banner preview of the deployed template (e.g.
    /// `/templates/nextjs-saas-starter.png`). Rendered as a wide preview on the
    /// template card; optional — templates without one show no banner.
    pub screenshot_url: Option<String>,
    /// Source repository shared by the services that don't declare their own.
    /// Always present as the source-of-truth / build fallback, even for
    /// image-based templates.
    pub git: GitRef,
    /// Tags/categories for filtering
    pub tags: Vec<String>,
    /// Feature highlights
    pub features: Vec<String>,
    /// Managed databases the template needs (e.g., ["postgres", "redis"]).
    pub databases: Vec<String>,
    /// Variables shared by every service. Created in a project, they become
    /// the project's shared variables.
    pub shared_env_vars: Vec<EnvVarTemplate>,
    /// The services the template creates; never empty.
    pub services: Vec<TemplateServiceSpec>,
    /// Path routes between the services.
    pub routes: Vec<TemplateRoute>,
    /// Whether the template is publicly visible
    pub is_public: bool,
    /// Whether the template is featured/promoted
    pub is_featured: bool,
    /// Sort order for display (lower = first)
    pub sort_order: i32,
}

static NO_SERVICE: Lazy<TemplateServiceSpec> = Lazy::new(TemplateServiceSpec::default);

impl ProjectTemplate {
    /// The first service. For a one-service template, the template's only
    /// service; decoding guarantees at least one.
    pub fn primary(&self) -> &TemplateServiceSpec {
        self.services.first().unwrap_or(&NO_SERVICE)
    }

    /// Mutable access to the first service.
    pub fn primary_mut(&mut self) -> Option<&mut TemplateServiceSpec> {
        self.services.first_mut()
    }

    /// A service by name.
    pub fn service(&self, name: &str) -> Option<&TemplateServiceSpec> {
        self.services.iter().find(|service| service.name == name)
    }

    /// Whether the template creates more than one service.
    pub fn is_multi_service(&self) -> bool {
        self.services.len() > 1
    }
}

/// Wire form of [`ProjectTemplate`] accepting both YAML shapes.
#[derive(Deserialize)]
struct RawProjectTemplate {
    slug: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    kind: TemplateKind,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    screenshot_url: Option<String>,
    git: GitRef,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    databases: Vec<String>,
    #[serde(default)]
    shared_env_vars: Vec<EnvVarTemplate>,
    #[serde(default)]
    services: Option<RawServices>,
    #[serde(default)]
    routes: Vec<TemplateRoute>,
    #[serde(default = "default_true")]
    is_public: bool,
    #[serde(default)]
    is_featured: bool,
    #[serde(default)]
    sort_order: i32,
    // The original one-service shape.
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    preset_config: Option<serde_json::Value>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(default)]
    resources: Option<TemplateResources>,
    #[serde(default)]
    exposed_port: Option<i32>,
    #[serde(default)]
    health_check_path: Option<String>,
    #[serde(default)]
    managed_service_bindings: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    env_vars: Vec<EnvVarTemplate>,
}

/// `services` holds service definitions in the current shape and managed
/// database types in the original one.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawServices {
    Databases(Vec<String>),
    Definitions(Vec<TemplateServiceSpec>),
}

impl TryFrom<RawProjectTemplate> for ProjectTemplate {
    type Error = String;

    fn try_from(raw: RawProjectTemplate) -> Result<Self, Self::Error> {
        let legacy_fields = [
            ("preset", raw.preset.is_some()),
            ("preset_config", raw.preset_config.is_some()),
            ("image", raw.image.is_some()),
            ("command", raw.command.is_some()),
            ("resources", raw.resources.is_some()),
            ("exposed_port", raw.exposed_port.is_some()),
            ("health_check_path", raw.health_check_path.is_some()),
            (
                "managed_service_bindings",
                !raw.managed_service_bindings.is_empty(),
            ),
            ("env_vars", !raw.env_vars.is_empty()),
        ];
        let (databases, mut services) = match raw.services {
            Some(RawServices::Definitions(definitions)) => {
                if let Some((field, _)) = legacy_fields.iter().find(|(_, present)| *present) {
                    return Err(format!(
                        "template '{}' lists its services, so '{field}' belongs to one of them, not to the template",
                        raw.slug
                    ));
                }
                if definitions.is_empty() {
                    return Err(format!("template '{}' has no services", raw.slug));
                }
                (raw.databases, definitions)
            }
            legacy => {
                let databases = match legacy {
                    Some(RawServices::Databases(databases)) => databases,
                    _ => Vec::new(),
                };
                if !raw.databases.is_empty() && !databases.is_empty() {
                    return Err(format!(
                        "template '{}' lists databases under both 'databases' and 'services'",
                        raw.slug
                    ));
                }
                let databases = if databases.is_empty() {
                    raw.databases
                } else {
                    databases
                };
                let Some(preset) = raw.preset else {
                    return Err(format!(
                        "template '{}' needs either a list of services or a top-level preset",
                        raw.slug
                    ));
                };
                let service = TemplateServiceSpec {
                    name: service_name_from_slug(&raw.slug),
                    git: None,
                    directory: None,
                    preset,
                    preset_config: raw.preset_config,
                    image: raw.image,
                    command: raw.command,
                    resources: raw.resources,
                    exposed_port: raw.exposed_port,
                    health_check_path: raw.health_check_path,
                    env_vars: raw.env_vars,
                    databases: Vec::new(),
                    database_bindings: raw.managed_service_bindings,
                };
                (databases, vec![service])
            }
        };

        // Make each service's linked databases explicit, so a snapshot says
        // exactly what the service was created with.
        let only_service = services.len() == 1;
        for service in &mut services {
            let mut linked: Vec<String> = Vec::new();
            let declared = if only_service && service.databases.is_empty() {
                databases.clone()
            } else {
                service.databases.clone()
            };
            for database in declared
                .into_iter()
                .chain(service.database_bindings.keys().cloned())
            {
                if !linked
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&database))
                {
                    linked.push(database);
                }
            }
            service.databases = linked;
        }

        Ok(Self {
            slug: raw.slug,
            name: raw.name,
            version: raw.version,
            kind: raw.kind,
            description: raw.description,
            image_url: raw.image_url,
            screenshot_url: raw.screenshot_url,
            git: raw.git,
            tags: raw.tags,
            features: raw.features,
            databases,
            shared_env_vars: raw.shared_env_vars,
            services,
            routes: raw.routes,
            is_public: raw.is_public,
            is_featured: raw.is_featured,
            sort_order: raw.sort_order,
        })
    }
}

/// Immutable app release attached to a service created from it.
///
/// The resolved definition is deliberately stored with the service. The live
/// catalog is only needed to discover a newer release; deployments, edits and
/// rollbacks continue to work if the catalog later changes or disappears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ServiceTemplateInstance {
    /// Catalog schema used to deserialize `template`.
    pub schema_version: String,
    /// Stable app family identifier.
    pub slug: String,
    /// Applied template release.
    pub version: String,
    /// Which of the template's services this one is. Absent on snapshots
    /// written before templates could have several services: those are the
    /// template's only service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Exact resolved release from which this service was created/upgraded.
    pub template: ProjectTemplate,
}

/// Snapshot schema written by this binary. Schema "2" snapshots (a single
/// service with its fields at the top level) still decode; see
/// [`ProjectTemplate`].
pub const SERVICE_TEMPLATE_SCHEMA_VERSION: &str = "3";

/// Snapshot schemas this binary can decode.
pub const SUPPORTED_SERVICE_TEMPLATE_SCHEMA_VERSIONS: &[&str] = &["2", "3"];

/// Template catalog schemas: "1" holds starters only, "2" added apps, "3"
/// added templates with several services. Every version decodes both YAML
/// shapes.
pub const SUPPORTED_CATALOG_SCHEMA_VERSIONS: &[&str] = &["1", "2", "3"];

fn catalog_schema_supports_apps(version: &str) -> bool {
    matches!(version, "2" | "3")
}

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
            service: None,
            template,
        }
    }

    /// The same release, attached to one of the template's services.
    pub fn for_service(mut self, service: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self
    }

    /// The definition of the service this snapshot is attached to.
    pub fn spec(&self) -> Option<&TemplateServiceSpec> {
        match self.service.as_deref() {
            Some(name) => self.template.service(name),
            None => self.template.services.first(),
        }
    }

    /// The variables this service is configured with: its own, plus the
    /// template's shared variables when it is the template's only service.
    /// (A template with several services puts its shared variables on the
    /// project instead.)
    pub fn service_variables(&self) -> Vec<EnvVarTemplate> {
        let own = self
            .spec()
            .map(|spec| spec.env_vars.as_slice())
            .unwrap_or_default();
        if self.template.is_multi_service() {
            own.to_vec()
        } else {
            self.template
                .shared_env_vars
                .iter()
                .chain(own)
                .cloned()
                .collect()
        }
    }

    /// Parse the persisted release identifier using Semantic Versioning.
    pub fn release_version(&self) -> Result<semver::Version, semver::Error> {
        semver::Version::parse(&self.version)
    }

    pub fn validate(&self) -> Result<(), ServiceTemplateInstanceError> {
        if !SUPPORTED_SERVICE_TEMPLATE_SCHEMA_VERSIONS.contains(&self.schema_version.as_str()) {
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
        if self.template.kind != TemplateKind::App {
            errors.push("Resolved definition must have kind 'app'".to_string());
        }
        if self.spec().is_none() {
            errors.push(format!(
                "Resolved definition has no service named '{}'",
                self.service.as_deref().unwrap_or_default()
            ));
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
    pub fn from_yaml(yaml: &str) -> Result<Self, yaml_serde::Error> {
        yaml_serde::from_str(yaml)
    }

    /// Serialize configuration to YAML string
    pub fn to_yaml(&self) -> Result<String, yaml_serde::Error> {
        yaml_serde::to_string(self)
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

/// Whether `image` is an immutable reference: `name@sha256:<64 hex>`
/// (optionally `name:tag@sha256:…`). Template deployments of every kind only
/// accept these, so a mutable tag can never silently change what a template
/// runs.
pub fn is_pinned_image_reference(image: &str) -> bool {
    image.rsplit_once('@').is_some_and(|(name, digest)| {
        !name.trim().is_empty()
            && digest.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit())
            })
    })
}

fn is_supported_git_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("git@")
}

/// Service names become part of created service names and of variable
/// references (`{{ services.<name>.url }}`), so they stay DNS-label shaped.
pub fn is_valid_service_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 63
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !name.ends_with('-')
}

/// The name of the only service of a template written before services had
/// names of their own: it was the template's slug.
///
/// Slugs were never restricted to DNS labels (`my_app`, `2fa` and 255
/// characters were all accepted), while service names must be. A slug that
/// already is a valid name is used as it is; any other is mapped to a valid
/// one deterministically, so the same snapshot always decodes to the same
/// name and stays valid. The template's own slug is not changed.
fn service_name_from_slug(slug: &str) -> String {
    if is_valid_service_name(slug) {
        return slug.to_string();
    }
    let mut name = String::with_capacity(slug.len().min(63));
    for ch in slug.chars() {
        let ch = ch.to_ascii_lowercase();
        let mapped = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if mapped != '-' || !name.ends_with('-') {
            name.push(mapped);
        }
    }
    let mut name = name.trim_matches('-').to_string();
    if name.is_empty() {
        name = "app".to_string();
    } else if !name.starts_with(|first: char| first.is_ascii_lowercase()) {
        name.insert_str(0, "app-");
    }
    // Every character is ASCII here, so truncating by bytes is safe.
    name.truncate(63);
    let trimmed = name.trim_end_matches('-').len();
    name.truncate(trimmed);
    name
}

/// Coarse shape check; the full rules (reserved paths, allowed characters)
/// are applied when the route is created.
fn is_plausible_route_prefix(prefix: &str) -> bool {
    prefix.len() > 1
        && prefix.starts_with('/')
        && !prefix.ends_with('/')
        && !prefix.contains("//")
        && !prefix
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

fn validate_env_var_templates<'a>(
    variables: &'a [EnvVarTemplate],
    names: &mut BTreeSet<&'a str>,
    errors: &mut Vec<String>,
) {
    for variable in variables {
        if variable.name.trim().is_empty() {
            errors.push("Environment variable name cannot be empty".to_string());
        } else if !names.insert(variable.name.as_str()) {
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
}

/// Presets accepted by project creation: the slugs of
/// `temps_presets::all_presets()`, which this crate cannot depend on. A test in
/// temps-presets fails when the two lists differ.
pub const VALID_PRESETS: &[&str] = &[
    "nextjs",
    "vite",
    "react-app",
    "rsbuild",
    "docusaurus",
    "rust",
    "go",
    "python",
    "java",
    "docker-compose",
    "dockerfile",
    "custom",
    "autopack",
    "nixpacks",
    "nixpacks-node",
    "nixpacks-python",
    "nixpacks-rust",
    "nixpacks-go",
    "nixpacks-java",
    "nixpacks-php",
    "nixpacks-ruby",
    "nixpacks-deno",
    "nixpacks-elixir",
    "nixpacks-csharp",
    "nixpacks-dart",
    "nixpacks-static",
    "nixpacks-swift",
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
        let template: ProjectTemplate = yaml_serde::from_str(yaml).map_err(|error| {
            TemplateConfigError::ParseError(format!(
                "Failed to parse bundled template '{path}': {error}"
            ))
        })?;
        let expected_kind = if path.starts_with("services/") {
            TemplateKind::App
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
        if !SUPPORTED_CATALOG_SCHEMA_VERSIONS.contains(&config.version.as_str()) {
            return Err(TemplateConfigError::ParseError(format!(
                "Unsupported template catalog schema '{}'; supported versions are {}",
                config.version,
                SUPPORTED_CATALOG_SCHEMA_VERSIONS.join(", ")
            )));
        }
        let contains_service_templates = config
            .templates
            .iter()
            .any(|template| template.kind == TemplateKind::App);
        if contains_service_templates && !catalog_schema_supports_apps(&config.version) {
            return Err(TemplateConfigError::ParseError(format!(
                "Service templates require catalog schema '2' or later; received '{}'",
                config.version
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

        if template.kind == TemplateKind::App {
            if template.version.trim().is_empty() {
                errors.push("Service template version cannot be empty".to_string());
            } else if let Err(error) = semver::Version::parse(&template.version) {
                errors.push(format!(
                    "Service template version '{}' is not valid Semantic Versioning: {error}",
                    template.version
                ));
            }
        }

        // Check for valid git URL
        if template.git.url.is_empty() {
            errors.push("Git URL cannot be empty".to_string());
        } else if !is_supported_git_url(&template.git.url) {
            errors.push(format!("Invalid git URL: {}", template.git.url));
        }

        // Validate databases against known list
        for database in &template.databases {
            let database_lower = database.to_lowercase();
            if !VALID_SERVICES.contains(&database_lower.as_str()) {
                errors.push(format!(
                    "Unknown service '{}'. Valid services are: {}",
                    database,
                    VALID_SERVICES.join(", ")
                ));
            }
        }

        if template.services.is_empty() {
            errors.push("A template needs at least one service".to_string());
        }
        let mut service_names = BTreeSet::new();
        for service in &template.services {
            if !is_valid_service_name(&service.name) {
                errors.push(format!(
                    "Service name '{}' must be 1-63 lowercase letters, digits or '-', starting with a letter",
                    service.name
                ));
            } else if !service_names.insert(service.name.as_str()) {
                errors.push(format!(
                    "Service '{}' is declared more than once",
                    service.name
                ));
            }
            let prefix = if template.is_multi_service() {
                format!("Service '{}': ", service.name)
            } else {
                String::new()
            };
            errors.extend(
                Self::validate_service(template, service)
                    .into_iter()
                    .map(|error| format!("{prefix}{error}")),
            );
        }
        for database in &template.databases {
            if !template.services.iter().any(|service| {
                service
                    .databases
                    .iter()
                    .any(|linked| linked.eq_ignore_ascii_case(database))
            }) {
                errors.push(format!(
                    "Database '{database}' is not linked to any service; list it in a service's databases or bindings"
                ));
            }
        }

        let mut shared_names = BTreeSet::new();
        validate_env_var_templates(&template.shared_env_vars, &mut shared_names, &mut errors);
        for service in &template.services {
            for variable in &service.env_vars {
                if shared_names.contains(variable.name.as_str()) {
                    errors.push(format!(
                        "Environment variable '{}' is declared both as shared and by service '{}'",
                        variable.name, service.name
                    ));
                }
            }
        }

        let mut route_keys = BTreeSet::new();
        for route in &template.routes {
            for (role, name) in [("source", &route.source), ("target", &route.target)] {
                if template.service(name).is_none() {
                    errors.push(format!(
                        "Route {} '{}' is {role} '{name}', which is not a service of the template",
                        route.source, route.prefix
                    ));
                }
            }
            if route.source == route.target {
                errors.push(format!(
                    "Route '{}' on '{}' points at the service it is on",
                    route.prefix, route.source
                ));
            }
            if !is_plausible_route_prefix(&route.prefix) {
                errors.push(format!(
                    "Route prefix '{}' must be a path such as /api",
                    route.prefix
                ));
            }
            if !route_keys.insert((route.source.as_str(), route.prefix.as_str())) {
                errors.push(format!(
                    "Route '{}' on '{}' is declared more than once",
                    route.prefix, route.source
                ));
            }
        }

        errors
    }

    /// Rules for one service of a template.
    fn validate_service(template: &ProjectTemplate, service: &TemplateServiceSpec) -> Vec<String> {
        let mut errors = Vec::new();

        if template.kind == TemplateKind::App {
            if service.is_image() {
                if service.preset != "dockerfile" {
                    errors.push(
                        "Services that run a prebuilt image must use the dockerfile preset"
                            .to_string(),
                    );
                }
                if service
                    .image
                    .as_deref()
                    .is_some_and(|image| !is_pinned_image_reference(image))
                {
                    errors.push(
                        "Service templates must use an immutable sha256 image digest".to_string(),
                    );
                }
                if service
                    .exposed_port
                    .is_none_or(|port| !(1..=65_535).contains(&port))
                {
                    errors.push(
                        "Service templates must declare an exposed port between 1 and 65535"
                            .to_string(),
                    );
                }
            } else if !template.is_multi_service() {
                // A one-service app runs a pinned image: that is what makes
                // its upgrades reviewable.
                errors.push("Service templates must declare a container image".to_string());
            } else if service.source(&template.git).r#ref.trim().is_empty() {
                errors.push(
                    "Services built from source must pin the reference they build".to_string(),
                );
            }
        } else if service.image.is_some() && !service.is_image() {
            errors.push("Image reference cannot be empty".to_string());
        }

        if let Some(git) = &service.git {
            if git.url.is_empty() || !is_supported_git_url(&git.url) {
                errors.push(format!("Invalid git URL: {}", git.url));
            }
        }
        if let Some(directory) = service.directory.as_deref() {
            if directory.trim().is_empty()
                || directory.starts_with('/')
                || directory.split('/').any(|segment| segment == "..")
                || directory.chars().any(char::is_control)
            {
                errors.push(format!(
                    "Directory '{directory}' must be a relative path inside the repository"
                ));
            }
        }

        let mut names = BTreeSet::new();
        validate_env_var_templates(&service.env_vars, &mut names, &mut errors);

        for database in &service.databases {
            if !template
                .databases
                .iter()
                .any(|declared| declared.eq_ignore_ascii_case(database))
            {
                errors.push(format!(
                    "Managed service bindings for '{database}' require it to be listed in services"
                ));
            }
        }
        if template.kind == TemplateKind::Starter && !service.database_bindings.is_empty() {
            // Bindings are applied at deploy time from an app's pinned release;
            // a starter's code is the user's, and reads the database's own
            // variables (POSTGRES_URL, REDIS_URL, …) directly.
            errors.push(
                "Database bindings are only applied for apps; a starter reads the database's variables directly"
                    .to_string(),
            );
        }
        for (database, bindings) in &service.database_bindings {
            for (target, source) in bindings {
                if target.trim().is_empty() || source.trim().is_empty() {
                    errors.push(format!(
                        "Managed service binding names for '{database}' cannot be empty"
                    ));
                }
            }
        }

        if let Some(command) = &service.command {
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

        if let Some(path) = service.health_check_path.as_deref() {
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

        if let Some(resources) = &service.resources {
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
        if service.preset.is_empty() {
            errors.push("Preset cannot be empty".to_string());
        } else if !VALID_PRESETS.contains(&service.preset.as_str()) {
            errors.push(format!(
                "Unknown preset '{}'. Valid presets are: {}",
                service.preset,
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
            .any(|template| template.kind == TemplateKind::App);
        if contains_service_templates && !catalog_schema_supports_apps(&additional_config.version) {
            return Err(TemplateConfigError::ParseError(format!(
                "Additional service templates require catalog schema '2' or later; received '{}'",
                additional_config.version
            )));
        }

        // Validate additional templates: warn and skip invalid ones instead of
        // aborting the whole file. An unknown preset is the most common case
        // (the operator pinned a custom preset slug); logging the valid list
        // makes the message actionable without stopping the rest of the file.
        let invalid_slugs: std::collections::HashSet<String> = {
            let errors = Self::validate_config(&additional_config);
            let mut set = std::collections::HashSet::new();
            for err in &errors {
                // Separate "unknown preset" errors (warn with the valid list)
                // from other validation failures (warn with the error text).
                let preset_errors: Vec<String> = err
                    .errors
                    .iter()
                    .filter(|e| e.starts_with("Unknown preset '"))
                    .cloned()
                    .collect();
                if !preset_errors.is_empty() {
                    warn!(
                        template_slug = %err.slug,
                        errors = ?preset_errors,
                        valid_presets = ?VALID_PRESETS,
                        "Skipping additional template: it uses an unrecognised preset. \
                         Valid presets are listed above"
                    );
                } else {
                    warn!(
                        template_slug = %err.slug,
                        errors = ?err.errors,
                        "Skipping additional template: validation failed"
                    );
                }
                set.insert(err.slug.clone());
            }
            set
        };

        // Merge with existing templates, skipping any that failed validation
        // or duplicate an existing slug.
        let mut write_guard = self.config.write().await;
        for template in additional_config.templates {
            if invalid_slugs.contains(&template.slug) {
                continue;
            }
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
        if !catalog_schema_supports_apps(&config.version) {
            return Err(TemplateConfigError::ParseError(format!(
                "Unsupported template catalog schema '{}'; apps need '2' or later",
                config.version
            )));
        }
        let template = config
            .get_by_slug(slug)
            .filter(|template| template.is_public && template.kind == TemplateKind::App)
            .cloned()
            .ok_or_else(|| TemplateConfigError::NotFound(slug.to_string()))?;
        let instance = ServiceTemplateInstance::new(SERVICE_TEMPLATE_SCHEMA_VERSION, template);
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

    fn only_service(template: &mut ProjectTemplate) -> &mut TemplateServiceSpec {
        template
            .services
            .first_mut()
            .expect("the template has a service")
    }

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
    preset: python
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
        assert_eq!(first.primary().preset, "nextjs");
        assert_eq!(first.primary().name, "nextjs-saas-starter");
        assert!(first.is_public);
        assert!(first.is_featured);
        assert_eq!(first.sort_order, 1);

        // Check Git ref
        assert_eq!(
            first.git.url,
            "https://github.com/temps-templates/nextjs-saas-starter.git"
        );
        assert_eq!(first.git.r#ref, "main");

        // The original shape's `services` are its managed databases, linked
        // to its only service.
        assert_eq!(first.databases, vec!["postgres", "redis"]);
        assert_eq!(first.services.len(), 1);
        assert_eq!(first.primary().databases, vec!["postgres", "redis"]);

        // Check env vars
        assert_eq!(first.primary().env_vars.len(), 3);
        assert_eq!(first.primary().env_vars[0].name, "NEXTAUTH_SECRET");
        assert!(first.primary().env_vars[0].required);

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
        assert!(t.databases.is_empty());
        assert!(t.primary().env_vars.is_empty());
        assert_eq!(t.git.r#ref, "main"); // default ref
    }

    #[test]
    fn bundled_keycloak_is_a_pinned_postgres_backed_service() {
        let template = bundled_template_by_slug("keycloak")
            .expect("Keycloak should be part of the reviewed catalog");

        assert_eq!(template.kind, TemplateKind::App);
        assert_eq!(
            template.image_url.as_deref(),
            Some("/templates/keycloak.svg")
        );
        let service = template.primary();
        assert_eq!(service.name, "keycloak");
        assert_eq!(
            service.image.as_deref(),
            Some("quay.io/keycloak/keycloak@sha256:9d1f1b2b7261ff53c66cb1092dfcdc34a5fb77e81f9e6a6e75b8b6a795de8067")
        );
        assert_eq!(service.command, Some(vec!["start".to_string()]));
        assert_eq!(
            service
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_limit),
            Some(1_000_000)
        );
        assert_eq!(
            service
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_limit),
            Some(1536)
        );
        assert_eq!(template.databases, vec!["postgres".to_string()]);
        assert_eq!(service.databases, vec!["postgres".to_string()]);
        assert_eq!(
            service
                .database_bindings
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

        assert_eq!(template.kind, TemplateKind::App);
        assert_eq!(
            template.image_url.as_deref(),
            Some("/templates/browserless.svg")
        );
        let service = template.primary();
        assert_eq!(
            service.image.as_deref(),
            Some("ghcr.io/browserless/chromium@sha256:6f3149efabf5e04a44385974448c85264398db416bc63f884564316d0fcadc3e")
        );
        assert_eq!(service.exposed_port, Some(3000));
        assert_eq!(service.health_check_path.as_deref(), Some("/docs"));
        assert!(template.databases.is_empty());

        let token = service
            .env_vars
            .iter()
            .find(|variable| variable.name == "TOKEN")
            .expect("Browserless must require an access token");
        assert!(token.required);
        assert_eq!(token.default_generator.as_deref(), Some("random_secret"));

        let external = service
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
        only_service(&mut template).image = Some("quay.io/keycloak/keycloak:26.7.2".to_string());

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
        let service = only_service(&mut template);
        service.image = Some(format!("@sha256:{}", "a".repeat(64)));
        service.command = Some(vec!["ok".to_string(); 65]);
        service.health_check_path = Some("https://attacker.test/ready".to_string());

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
        let resources = only_service(&mut template)
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
                tags: vec!["test".to_string()],
                features: vec!["Feature 1".to_string()],
                databases: vec!["postgres".to_string()],
                shared_env_vars: vec![],
                services: vec![TemplateServiceSpec {
                    name: "web".to_string(),
                    preset: "nextjs".to_string(),
                    databases: vec!["postgres".to_string()],
                    ..Default::default()
                }],
                routes: vec![],
                is_public: true,
                is_featured: false,
                sort_order: 0,
            }],
        };

        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("slug: test"));
        assert!(yaml.contains("name: Test Template"));
        assert!(yaml.contains("https://github.com/test/test-repo.git"));

        // What is written reads back unchanged.
        let reparsed = TemplatesConfig::from_yaml(&yaml).expect("serialized catalog parses");
        assert_eq!(reparsed.templates, config.templates);
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
            starter.databases.iter().any(|s| s == "postgres"),
            "observability-starter must depend on postgres"
        );
        assert!(
            starter.primary().env_vars.is_empty(),
            "observability-starter must not ask for platform-managed observability variables"
        );
    }

    /// Template deployments reject mutable image tags, so a curated template
    /// with one could never be deployed (it failed the first-run demo with
    /// "use an immutable image reference ending in @sha256:…").
    #[test]
    fn bundled_template_images_are_pinned_by_digest() {
        let config = bundled_templates_config().expect("bundled templates must parse");
        let unpinned: Vec<String> = config
            .templates
            .iter()
            .filter_map(|template| {
                let image = template.primary().image.as_deref()?.trim();
                (!image.is_empty() && !is_pinned_image_reference(image))
                    .then(|| format!("{}: {}", template.slug, image))
            })
            .collect();
        assert!(
            unpinned.is_empty(),
            "bundled templates must pin images as name@sha256:<64 hex>; unpinned: {:?}",
            unpinned
        );

        let starter = config
            .get_by_slug("observability-starter")
            .expect("observability-starter template must exist");
        assert!(
            starter
                .primary()
                .image
                .as_deref()
                .is_some_and(is_pinned_image_reference),
            "the first-run demo must deploy from a pinned image"
        );
    }

    #[test]
    fn pinned_image_reference_rules() {
        let digest = "a".repeat(64);
        assert!(is_pinned_image_reference(&format!(
            "ghcr.io/example/app@sha256:{digest}"
        )));
        assert!(is_pinned_image_reference(&format!(
            "ghcr.io/example/app:1.2.3@sha256:{digest}"
        )));
        assert!(!is_pinned_image_reference("ghcr.io/example/app:latest"));
        assert!(!is_pinned_image_reference(&format!("@sha256:{digest}")));
        assert!(!is_pinned_image_reference("ghcr.io/example/app@sha256:abc"));
        assert!(!is_pinned_image_reference(&format!(
            "ghcr.io/example/app@sha512:{digest}"
        )));
        assert!(!is_pinned_image_reference(&format!(
            "ghcr.io/example/app@sha256:{}",
            "g".repeat(64)
        )));
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
        assert_eq!(instance.schema_version, "3");
        assert_eq!(instance.template.kind, TemplateKind::App);
        assert!(instance.template.primary().image.is_some());
        assert_eq!(
            instance.spec().map(|spec| spec.name.as_str()),
            Some("keycloak")
        );
    }

    #[test]
    fn service_template_instance_rejects_an_invalid_resolved_definition() {
        let mut template = bundled_template_by_slug("browserless")
            .expect("Browserless should be part of the reviewed catalog");
        only_service(&mut template).image =
            Some("ghcr.io/browserless/chromium:v2.56.0".to_string());
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
        let service = only_service(&mut template);
        service.env_vars.push(service.env_vars[0].clone());

        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error == "Service template version cannot be empty"));
        assert!(errors
            .iter()
            .any(|error| error.contains("declared more than once")));

        template.version = "next".to_string();
        only_service(&mut template).env_vars.pop();
        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error.contains("not valid Semantic Versioning")));

        template.version = "1.0.0".to_string();
        only_service(&mut template).image = Some("example.test/keycloak:latest".to_string());
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
            .primary()
            .env_vars
            .iter()
            .find(|variable| variable.name == "KC_BOOTSTRAP_ADMIN_PASSWORD")
            .expect("password input must exist");
        let username = template
            .primary()
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
        let password = only_service(&mut template)
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
    preset: python

  - slug: bitbucket-template
    name: Bitbucket Template
    git:
      url: https://bitbucket.org/owner/repo.git
      path: packages/app
      ref: v1.0.0
    preset: nixpacks-node
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

    const MULTI_SERVICE: &str = r#"
slug: secrets-manager
name: Secrets manager
version: 1.0.0
kind: app
git:
  url: https://github.com/example/secrets.git
  ref: v1.2.3
databases:
  - postgres
  - redis
shared_env_vars:
  - name: ENCRYPTION_KEY
    secret: true
    default_generator: random_hex_32
services:
  - name: backend
    directory: backend
    preset: dockerfile
    exposed_port: 8080
    health_check_path: /api/status
    resources:
      memory_limit: 2048
    database_bindings:
      postgres:
        DB_CONNECTION_URI: POSTGRES_URL
      redis:
        REDIS_URL: REDIS_URL
  - name: frontend
    directory: frontend
    preset: vite
routes:
  - source: frontend
    prefix: /api
    target: backend
"#;

    #[test]
    fn a_template_can_list_several_services() {
        let template: ProjectTemplate =
            yaml_serde::from_str(MULTI_SERVICE).expect("multi-service template parses");

        assert_eq!(template.kind, TemplateKind::App);
        assert!(template.is_multi_service());
        assert_eq!(template.databases, vec!["postgres", "redis"]);
        let backend = template.service("backend").expect("backend service");
        assert_eq!(backend.databases, vec!["postgres", "redis"]);
        assert_eq!(
            backend.source(&template.git).path.as_deref(),
            Some("backend")
        );
        assert_eq!(backend.source(&template.git).r#ref, "v1.2.3");
        let frontend = template.service("frontend").expect("frontend service");
        assert!(
            frontend.databases.is_empty(),
            "a service of several is linked only to the databases it asks for"
        );
        assert_eq!(template.routes[0].target, "backend");
        assert!(!template.routes[0].strip_prefix);
        assert!(
            TemplateService::validate_template(&template).is_empty(),
            "{:?}",
            TemplateService::validate_template(&template)
        );

        // Serializing keeps the shape, so snapshots round-trip.
        let json = serde_json::to_value(&template).expect("serialize");
        let back: ProjectTemplate = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, template);
    }

    #[test]
    fn service_is_accepted_as_the_former_name_of_app() {
        let kind: TemplateKind = serde_json::from_str("\"service\"").expect("alias");
        assert_eq!(kind, TemplateKind::App);
        assert_eq!(serde_json::to_string(&kind).expect("serialize"), "\"app\"");
    }

    #[test]
    fn schema_two_snapshots_still_decode_as_their_only_service() {
        let snapshot = serde_json::json!({
            "schema_version": "2",
            "slug": "keycloak",
            "version": "1.0.0",
            "template": {
                "slug": "keycloak",
                "name": "Keycloak",
                "version": "1.0.0",
                "kind": "service",
                "git": {"url": "https://github.com/keycloak/keycloak.git", "ref": "26.7.2"},
                "preset": "dockerfile",
                "image": format!("quay.io/keycloak/keycloak@sha256:{}", "a".repeat(64)),
                "exposed_port": 8080,
                "services": ["postgres"],
                "managed_service_bindings": {"postgres": {"KC_DB_URL_HOST": "POSTGRES_HOST"}},
                "env_vars": [{"name": "KC_DB", "default": "postgres", "required": true}]
            }
        });
        let instance: ServiceTemplateInstance =
            serde_json::from_value(snapshot).expect("schema-two snapshot decodes");

        assert!(instance.validate().is_ok(), "{:?}", instance.validate());
        let spec = instance.spec().expect("the only service");
        assert_eq!(spec.name, "keycloak");
        assert_eq!(spec.databases, vec!["postgres"]);
        assert_eq!(
            spec.database_bindings["postgres"]["KC_DB_URL_HOST"],
            "POSTGRES_HOST"
        );
        assert_eq!(spec.env_vars[0].name, "KC_DB");
    }

    fn legacy_snapshot(slug: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": "2",
            "slug": slug,
            "version": "1.0.0",
            "template": {
                "slug": slug,
                "name": "Legacy App",
                "version": "1.0.0",
                "kind": "service",
                "git": {"url": "https://github.com/example/legacy.git", "ref": "main"},
                "preset": "dockerfile",
                "image": format!("registry.example.test/legacy@sha256:{}", "b".repeat(64)),
                "exposed_port": 8080,
            }
        })
    }

    #[test]
    fn legacy_snapshots_with_non_dns_slugs_stay_valid() {
        let long_slug = "a".repeat(64);
        for (slug, expected) in [
            ("my_app", "my-app"),
            ("2fa", "app-2fa"),
            ("My App!!", "my-app"),
            ("___", "app"),
            (long_slug.as_str(), &long_slug[..63]),
            ("plain", "plain"),
        ] {
            let instance: ServiceTemplateInstance =
                serde_json::from_value(legacy_snapshot(slug)).expect("snapshot decodes");
            assert_eq!(instance.slug, slug, "the template slug is untouched");
            assert_eq!(instance.template.slug, slug);
            assert_eq!(
                instance.spec().map(|spec| spec.name.as_str()),
                Some(expected),
                "slug {slug:?}"
            );
            assert!(
                instance.validate().is_ok(),
                "slug {slug:?}: {:?}",
                instance.validate()
            );

            // The derived name is stable across a persist/reload cycle.
            let json = serde_json::to_value(&instance).expect("serialize");
            let back: ServiceTemplateInstance =
                serde_json::from_value(json).expect("persisted snapshot decodes");
            assert_eq!(back, instance, "slug {slug:?}");
            assert!(back.validate().is_ok(), "slug {slug:?}");
        }
    }

    #[test]
    fn slug_derived_service_names_are_always_valid_and_deterministic() {
        for slug in [
            "x",
            "9",
            "-",
            "a-",
            "-a",
            "Ünïcode",
            "a b c",
            "UPPER",
            "dots.and_underscores",
            &"9".repeat(255),
            &format!("{}-{}", "a".repeat(62), "b"),
        ] {
            let name = service_name_from_slug(slug);
            assert!(is_valid_service_name(&name), "{slug:?} -> {name:?}");
            assert_eq!(name, service_name_from_slug(slug));
        }
    }

    #[test]
    fn explicit_service_names_are_still_validated() {
        let template: ProjectTemplate =
            yaml_serde::from_str(MULTI_SERVICE).expect("multi-service template parses");
        let mut template = template;
        template.services[0].name = "Not_A_Label".to_string();
        let errors = TemplateService::validate_template(&template);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("Not_A_Label") && error.contains("lowercase")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_snapshot_names_the_service_it_belongs_to() {
        let template: ProjectTemplate =
            yaml_serde::from_str(MULTI_SERVICE).expect("multi-service template parses");
        let instance = ServiceTemplateInstance::new(SERVICE_TEMPLATE_SCHEMA_VERSION, template)
            .for_service("frontend");
        assert_eq!(
            instance.spec().map(|spec| spec.preset.as_str()),
            Some("vite")
        );

        let unknown = instance.clone().for_service("worker");
        assert!(matches!(
            unknown.validate(),
            Err(ServiceTemplateInstanceError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn service_fields_cannot_also_sit_at_the_top_level() {
        let yaml = r#"
slug: mixed
name: Mixed
git:
  url: https://github.com/example/mixed.git
preset: nextjs
services:
  - name: web
    preset: nextjs
"#;
        let error = yaml_serde::from_str::<ProjectTemplate>(yaml)
            .expect_err("a template cannot use both shapes");
        assert!(error
            .to_string()
            .contains("'preset' belongs to one of them"));
    }

    #[test]
    fn references_between_services_are_checked() {
        let mut template: ProjectTemplate =
            yaml_serde::from_str(MULTI_SERVICE).expect("multi-service template parses");
        template.routes.push(TemplateRoute {
            source: "frontend".to_string(),
            prefix: "/admin".to_string(),
            target: "worker".to_string(),
            strip_prefix: false,
        });
        template.services[1].name = "backend".to_string();
        template.services[1].env_vars.push(EnvVarTemplate {
            name: "ENCRYPTION_KEY".to_string(),
            example: None,
            default: None,
            description: None,
            required: false,
            secret: true,
            default_generator: None,
        });
        template.databases.push("mongodb".to_string());

        let errors = TemplateService::validate_template(&template);
        for expected in [
            "is not a service of the template",
            "Service 'backend' is declared more than once",
            "declared both as shared and by service",
            "Database 'mongodb' is not linked to any service",
        ] {
            assert!(
                errors.iter().any(|error| error.contains(expected)),
                "missing '{expected}' in {errors:?}"
            );
        }
    }

    #[test]
    fn services_of_an_app_may_build_from_a_pinned_source() {
        let mut template: ProjectTemplate =
            yaml_serde::from_str(MULTI_SERVICE).expect("multi-service template parses");
        assert!(template.services.iter().all(|service| !service.is_image()));
        assert!(TemplateService::validate_template(&template).is_empty());

        // A one-service app still runs a pinned image.
        template.services.truncate(1);
        template.routes.clear();
        let errors = TemplateService::validate_template(&template);
        assert!(errors
            .iter()
            .any(|error| error == "Service templates must declare a container image"));
    }
}
