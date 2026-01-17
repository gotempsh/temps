//! Project Templates Configuration
//!
//! Curated project templates that users can use to quickly create new projects.
//! Templates are defined in a YAML configuration file for easy customization.

use serde::{Deserialize, Serialize};
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
}

/// Git repository reference (supports any git provider: GitHub, GitLab, Bitbucket, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct GitRef {
    /// Git repository URL (e.g., "https://github.com/owner/repo.git" or "https://gitlab.com/owner/repo.git")
    pub url: String,
    /// Path within the repository (for monorepos)
    #[serde(default)]
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

/// A curated project template
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ProjectTemplate {
    /// Unique identifier for the template (used in URLs)
    pub slug: String,
    /// Display name
    pub name: String,
    /// Short description
    #[serde(default)]
    pub description: Option<String>,
    /// URL to template image/icon
    #[serde(default)]
    pub image_url: Option<String>,
    /// Git repository reference (supports any git provider)
    pub git: GitRef,
    /// Framework/preset to use (e.g., "nextjs", "fastapi", "dockerfile")
    pub preset: String,
    /// Preset-specific configuration
    #[serde(default)]
    pub preset_config: Option<serde_json::Value>,
    /// Tags/categories for filtering
    #[serde(default)]
    pub tags: Vec<String>,
    /// Feature highlights
    #[serde(default)]
    pub features: Vec<String>,
    /// Required external services (e.g., ["postgres", "redis"])
    #[serde(default)]
    pub services: Vec<String>,
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

        // Check for empty preset
        if template.preset.is_empty() {
            errors.push("Preset cannot be empty".to_string());
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
        templates.sort_by(|a, b| a.sort_order.cmp(&b.sort_order));
        templates
    }

    /// Get featured templates
    pub async fn list_featured_templates(&self) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config.featured_templates().into_iter().cloned().collect();
        templates.sort_by(|a, b| a.sort_order.cmp(&b.sort_order));
        templates
    }

    /// Get templates filtered by tag
    pub async fn list_templates_by_tag(&self, tag: &str) -> Vec<ProjectTemplate> {
        let config = self.config.read().await;
        let mut templates: Vec<_> = config.templates_by_tag(tag).into_iter().cloned().collect();
        templates.sort_by(|a, b| a.sort_order.cmp(&b.sort_order));
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
        assert!(t.is_public); // default
        assert!(!t.is_featured); // default
        assert_eq!(t.sort_order, 0); // default
        assert!(t.tags.is_empty());
        assert!(t.services.is_empty());
        assert!(t.env_vars.is_empty());
        assert_eq!(t.git.r#ref, "main"); // default ref
    }

    #[test]
    fn test_serialize_config() {
        let config = TemplatesConfig {
            version: "1".to_string(),
            templates: vec![ProjectTemplate {
                slug: "test".to_string(),
                name: "Test Template".to_string(),
                description: Some("A test template".to_string()),
                image_url: None,
                git: GitRef {
                    url: "https://github.com/test/test-repo.git".to_string(),
                    path: None,
                    r#ref: "main".to_string(),
                },
                preset: "nextjs".to_string(),
                preset_config: None,
                tags: vec!["test".to_string()],
                features: vec!["Feature 1".to_string()],
                services: vec!["postgres".to_string()],
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
