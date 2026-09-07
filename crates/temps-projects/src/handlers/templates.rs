// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Template Handlers
//!
//! HTTP handlers for template-related endpoints.

use serde::{Deserialize, Serialize};
use temps_core::templates::{EnvVarTemplate, ProjectTemplate, TemplateKind, TemplateResources};
use utoipa::ToSchema;

/// Query parameters for listing templates
#[derive(Debug, Deserialize, ToSchema)]
pub struct ListTemplatesQuery {
    /// Filter templates by tag
    pub tag: Option<String>,
    /// Only return featured templates
    pub featured: Option<bool>,
    /// Filter by gallery (`starter` or `service`).
    pub kind: Option<TemplateKind>,
}

/// Response type for a single template
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateResponse {
    /// Unique identifier for the template (used in URLs)
    pub slug: String,
    /// Display name
    pub name: String,
    /// Immutable release identifier for service templates.
    pub version: String,
    /// Gallery this template belongs to.
    pub kind: TemplateKind,
    /// Short description
    pub description: Option<String>,
    /// URL to template image/icon
    pub image_url: Option<String>,
    /// URL to a wide screenshot/banner preview of the deployed template.
    /// Absent for templates that don't have one captured yet.
    pub screenshot_url: Option<String>,
    /// Git repository reference
    pub git: GitRefResponse,
    /// Framework/preset to use
    pub preset: String,
    /// Tags/categories for filtering
    pub tags: Vec<String>,
    /// Feature highlights
    pub features: Vec<String>,
    /// Required external services
    pub services: Vec<String>,
    /// Environment variables template
    pub env_vars: Vec<EnvVarTemplateResponse>,
    /// Whether the template is featured/promoted
    pub is_featured: bool,
    /// Prebuilt Docker image reference. When set, the one-click deploy pulls and
    /// runs this image directly (no build); when absent it builds from `git`.
    pub image: Option<String>,
    /// Optional command passed to the image entrypoint.
    pub command: Option<Vec<String>>,
    /// Curated CPU/memory profile applied when the project is created.
    pub resources: Option<TemplateResources>,
    /// Container port the prebuilt image listens on (image deploys only).
    pub exposed_port: Option<i32>,
    /// HTTP health-check path probed after the container starts (image deploys).
    pub health_check_path: Option<String>,
    /// Managed-service environment aliases used at deployment time.
    pub managed_service_bindings:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// Git repository reference response
#[derive(Debug, Serialize, ToSchema)]
pub struct GitRefResponse {
    /// Git repository URL
    pub url: String,
    /// Path within the repository (for monorepos)
    pub path: Option<String>,
    /// Git reference (branch, tag, or commit)
    pub r#ref: String,
}

/// Environment variable template response
#[derive(Debug, Serialize, ToSchema)]
pub struct EnvVarTemplateResponse {
    /// Name of the environment variable
    pub name: String,
    /// Example value for documentation
    pub example: Option<String>,
    /// Default value if not provided by user
    pub default: Option<String>,
    /// Description of what this variable is used for
    pub description: Option<String>,
    /// Whether this variable is required
    pub required: bool,
    /// Whether values must use the protected secret reveal path.
    pub secret: bool,
    /// Frontend-side generator hint for the default value
    /// (e.g. `app_url`, `random_secret`, `random_hex_32`)
    pub default_generator: Option<String>,
}

impl From<ProjectTemplate> for TemplateResponse {
    fn from(template: ProjectTemplate) -> Self {
        Self {
            slug: template.slug,
            name: template.name,
            version: template.version,
            kind: template.kind,
            description: template.description,
            image_url: template.image_url,
            screenshot_url: template.screenshot_url,
            git: GitRefResponse {
                url: template.git.url,
                path: template.git.path,
                r#ref: template.git.r#ref,
            },
            preset: template.preset,
            tags: template.tags,
            features: template.features,
            services: template.services,
            env_vars: template
                .env_vars
                .into_iter()
                .map(EnvVarTemplateResponse::from)
                .collect(),
            is_featured: template.is_featured,
            image: template.image,
            command: template.command,
            resources: template.resources,
            exposed_port: template.exposed_port,
            health_check_path: template.health_check_path,
            managed_service_bindings: template.managed_service_bindings,
        }
    }
}

impl From<EnvVarTemplate> for EnvVarTemplateResponse {
    fn from(env_var: EnvVarTemplate) -> Self {
        let secret = env_var.is_secret();
        Self {
            name: env_var.name,
            example: env_var.example,
            default: if secret { None } else { env_var.default },
            description: env_var.description,
            required: env_var.required,
            secret,
            default_generator: env_var.default_generator,
        }
    }
}

/// Response for listing templates
#[derive(Debug, Serialize, ToSchema)]
pub struct ListTemplatesResponse {
    /// List of templates
    pub templates: Vec<TemplateResponse>,
    /// Total number of templates
    pub total: usize,
}

/// Response for listing tags
#[derive(Debug, Serialize, ToSchema)]
pub struct ListTagsResponse {
    /// List of available tags
    pub tags: Vec<String>,
    /// Total number of tags
    pub total: usize,
}

/// Request to create a project from a template
///
/// Supports three deploy modes:
///   * **Native image service mode** — curated service templates deploy a
///     digest-pinned container image and retain their template release identity,
///     runtime configuration, and managed-service bindings.
///   * **Fork mode** — when `git_provider_connection_id` is set, the template
///     repo is cloned into a new repository under the user's Git account and the
///     project tracks that fork (git-push deploys, automatic deploy on push).
///   * **One-click public-repo mode** — when `git_provider_connection_id` is
///     omitted, the project deploys directly from the template's public source
///     repository (no fork, no Git account required). This is the activation
///     path: a brand-new user with no Git provider connected can still deploy a
///     demo in one click. `repository_name` / `repository_owner` are ignored in
///     this mode, and automatic-deploy-on-push is unavailable (there is no fork
///     to receive webhooks).
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProjectFromTemplateRequest {
    /// Template slug to use as the base
    pub template_slug: String,
    /// Name for the new project
    pub project_name: String,
    /// Git provider connection ID. When omitted, the project deploys directly
    /// from the template's public source repository instead of forking it.
    #[serde(default)]
    pub git_provider_connection_id: Option<i32>,
    /// Name for the new repository to create. Required in fork mode; ignored in
    /// one-click public-repo mode.
    #[serde(default)]
    pub repository_name: Option<String>,
    /// Owner/organization for the new repository (defaults to authenticated user)
    pub repository_owner: Option<String>,
    /// Whether to make the repository private (defaults to true)
    #[serde(default = "default_private")]
    pub private: bool,
    /// Environment variables to set (key-value pairs)
    #[serde(default)]
    pub environment_variables: Vec<EnvVarInput>,
    /// External storage service IDs to attach to the project
    #[serde(default)]
    pub storage_service_ids: Vec<i32>,
    /// Enable automatic deployment on push (defaults to true). Only honoured in
    /// fork mode; public-repo deploys cannot receive push webhooks.
    #[serde(default = "default_true")]
    pub automatic_deploy: bool,
    /// Optional prebuilt-image override. Curated template values remain the
    /// default when this is omitted. Accepted only for image templates.
    #[serde(default)]
    pub image: Option<String>,
    /// Optional image entrypoint arguments. An empty list explicitly uses the
    /// image's own default command instead of the template command.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// CPU request override in microcores (1_000_000 = one CPU core).
    #[serde(default)]
    pub cpu_request: Option<i32>,
    /// CPU limit override in microcores. Zero means uncapped.
    #[serde(default)]
    pub cpu_limit: Option<i32>,
    /// Memory request override in MiB.
    #[serde(default)]
    pub memory_request: Option<i32>,
    /// Memory limit override in MiB. Zero means uncapped.
    #[serde(default)]
    pub memory_limit: Option<i32>,
    /// Public container port override.
    #[serde(default)]
    pub exposed_port: Option<i32>,
    /// Relative HTTP health-check path override.
    #[serde(default)]
    pub health_check_path: Option<String>,
}

fn default_private() -> bool {
    true
}

fn default_true() -> bool {
    true
}

/// Input for environment variable
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct EnvVarInput {
    /// Variable name
    pub name: String,
    /// Variable value
    pub value: String,
    /// Mark the variable as a secret. Secret values are encrypted at rest,
    /// masked in list responses, and revealable only through an audited,
    /// permission-checked endpoint. Defaults to `false`.
    #[serde(default)]
    pub is_secret: bool,
}

/// Response after creating a project from template
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateProjectFromTemplateResponse {
    /// ID of the created project
    pub project_id: i32,
    /// Slug of the created project
    pub project_slug: String,
    /// Name of the created project
    pub project_name: String,
    /// URL of the created repository
    pub repository_url: String,
    /// Template that was used
    pub template_slug: String,
    /// Message with additional info
    pub message: String,
    /// Whether the initial deployment was successfully queued. This is set for
    /// native image service templates; Git-backed modes use their pipeline flow.
    pub deployment_queued: Option<bool>,
    /// Actionable retry guidance when project creation succeeded but deployment
    /// dispatch did not. Internal queue errors are never exposed.
    pub deployment_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::templates::{GitRef, TemplateService, TemplatesConfig};

    fn create_test_template() -> ProjectTemplate {
        ProjectTemplate {
            slug: "test-template".to_string(),
            name: "Test Template".to_string(),
            version: "1.0.0".to_string(),
            kind: TemplateKind::Starter,
            description: Some("A test template".to_string()),
            image_url: Some("/templates/test.png".to_string()),
            screenshot_url: Some("/templates/test-screenshot.png".to_string()),
            git: GitRef {
                url: "https://github.com/test/test-repo.git".to_string(),
                path: None,
                r#ref: "main".to_string(),
            },
            preset: "nextjs".to_string(),
            preset_config: None,
            resources: None,
            image: None,
            command: None,
            exposed_port: None,
            health_check_path: None,
            tags: vec!["test".to_string(), "example".to_string()],
            features: vec!["Feature 1".to_string(), "Feature 2".to_string()],
            services: vec!["postgres".to_string()],
            managed_service_bindings: Default::default(),
            env_vars: vec![EnvVarTemplate {
                name: "TEST_VAR".to_string(),
                example: Some("test_value".to_string()),
                default: None,
                description: Some("A test variable".to_string()),
                required: true,
                secret: false,
                default_generator: None,
            }],
            is_public: true,
            is_featured: true,
            sort_order: 1,
        }
    }

    #[test]
    fn test_template_response_from_project_template() {
        let template = create_test_template();
        let response = TemplateResponse::from(template.clone());

        assert_eq!(response.slug, "test-template");
        assert_eq!(response.name, "Test Template");
        assert_eq!(response.description, Some("A test template".to_string()));
        assert_eq!(
            response.screenshot_url,
            Some("/templates/test-screenshot.png".to_string())
        );
        assert_eq!(response.git.url, "https://github.com/test/test-repo.git");
        assert_eq!(response.git.r#ref, "main");
        assert_eq!(response.preset, "nextjs");
        assert_eq!(response.tags.len(), 2);
        assert_eq!(response.features.len(), 2);
        assert_eq!(response.services.len(), 1);
        assert_eq!(response.env_vars.len(), 1);
        assert!(response.is_featured);
    }

    #[test]
    fn test_env_var_template_response_from() {
        let env_var = EnvVarTemplate {
            name: "DATABASE_URL".to_string(),
            example: Some("postgres://localhost/db".to_string()),
            default: Some("postgres://localhost/default".to_string()),
            description: Some("Database connection URL".to_string()),
            required: true,
            secret: false,
            default_generator: None,
        };

        let response = EnvVarTemplateResponse::from(env_var);

        assert_eq!(response.name, "DATABASE_URL");
        assert!(response.secret);
        assert_eq!(response.default, None);
        assert_eq!(
            response.example,
            Some("postgres://localhost/db".to_string())
        );
        assert_eq!(
            response.description,
            Some("Database connection URL".to_string())
        );
        assert!(response.required);
    }

    #[tokio::test]
    async fn test_template_service_integration() {
        let service = TemplateService::new(None).expect("bundled templates must load");

        // Create test config
        let yaml = r#"
version: "1"
templates:
  - slug: test-1
    name: Test Template 1
    git:
      url: https://github.com/test/repo1.git
    preset: nextjs
    tags:
      - fullstack
      - typescript
    is_public: true
    is_featured: true
    sort_order: 1

  - slug: test-2
    name: Test Template 2
    git:
      url: https://gitlab.com/test/repo2.git
    preset: fastapi
    tags:
      - backend
      - python
    is_public: true
    is_featured: false
    sort_order: 2
"#;

        let config = TemplatesConfig::from_yaml(yaml).unwrap();
        service.set_config(config).await;

        // Test list_templates
        let templates = service.list_templates().await;
        assert_eq!(templates.len(), 2);

        assert!(templates[0].is_featured);
        assert!(templates[1]
            .tags
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case("python")));

        // Test get_template
        let template = service.get_template("test-1").await.unwrap();
        assert_eq!(template.name, "Test Template 1");

        // Test list_tags
        let tags = service.list_tags().await;
        assert!(tags.contains(&"fullstack".to_string()));
        assert!(tags.contains(&"python".to_string()));
    }
}
