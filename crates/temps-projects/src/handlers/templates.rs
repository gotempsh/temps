//! Template Handlers
//!
//! HTTP handlers for template-related endpoints.

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::RequireAuth;
use temps_core::{
    problemdetails::{self, Problem},
    templates::{EnvVarTemplate, ProjectTemplate, TemplateService},
};
use utoipa::{OpenApi, ToSchema};

/// State for template handlers
pub struct TemplateAppState {
    pub template_service: Arc<TemplateService>,
}

/// Query parameters for listing templates
#[derive(Debug, Deserialize, ToSchema)]
pub struct ListTemplatesQuery {
    /// Filter templates by tag
    pub tag: Option<String>,
    /// Only return featured templates
    pub featured: Option<bool>,
}

/// Response type for a single template
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateResponse {
    /// Unique identifier for the template (used in URLs)
    pub slug: String,
    /// Display name
    pub name: String,
    /// Short description
    pub description: Option<String>,
    /// URL to template image/icon
    pub image_url: Option<String>,
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
}

impl From<ProjectTemplate> for TemplateResponse {
    fn from(template: ProjectTemplate) -> Self {
        Self {
            slug: template.slug,
            name: template.name,
            description: template.description,
            image_url: template.image_url,
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
        }
    }
}

impl From<EnvVarTemplate> for EnvVarTemplateResponse {
    fn from(env_var: EnvVarTemplate) -> Self {
        Self {
            name: env_var.name,
            example: env_var.example,
            default: env_var.default,
            description: env_var.description,
            required: env_var.required,
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
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProjectFromTemplateRequest {
    /// Template slug to use as the base
    pub template_slug: String,
    /// Name for the new project
    pub project_name: String,
    /// Git provider connection ID (required to create the repository)
    pub git_provider_connection_id: i32,
    /// Name for the new repository to create
    pub repository_name: String,
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
    /// Enable automatic deployment on push (defaults to true)
    #[serde(default = "default_true")]
    pub automatic_deploy: bool,
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
}

/// Configure template routes
pub fn configure_routes() -> Router<Arc<TemplateAppState>> {
    Router::new()
        .route("/templates", get(list_templates))
        .route("/templates/tags", get(list_tags))
        .route("/templates/{slug}", get(get_template))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_templates,
        get_template,
        list_tags,
    ),
    components(
        schemas(
            ListTemplatesQuery,
            TemplateResponse,
            GitRefResponse,
            EnvVarTemplateResponse,
            ListTemplatesResponse,
            ListTagsResponse,
            CreateProjectFromTemplateRequest,
            EnvVarInput,
            CreateProjectFromTemplateResponse,
        )
    ),
    tags(
        (name = "Templates", description = "Project template endpoints")
    )
)]
pub struct TemplatesApiDoc;

/// List all available templates
///
/// Returns a list of all public templates, optionally filtered by tag or featured status.
#[utoipa::path(
    get,
    path = "/templates",
    tag = "Templates",
    params(
        ("tag" = Option<String>, Query, description = "Filter templates by tag"),
        ("featured" = Option<bool>, Query, description = "Only return featured templates")
    ),
    responses(
        (status = 200, description = "List of templates", body = ListTemplatesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_templates(
    State(state): State<Arc<TemplateAppState>>,
    RequireAuth(_auth): RequireAuth,
    Query(query): Query<ListTemplatesQuery>,
) -> Result<impl IntoResponse, Problem> {
    let templates = if let Some(true) = query.featured {
        state.template_service.list_featured_templates().await
    } else if let Some(tag) = query.tag {
        state.template_service.list_templates_by_tag(&tag).await
    } else {
        state.template_service.list_templates().await
    };

    let total = templates.len();
    let response = ListTemplatesResponse {
        templates: templates.into_iter().map(TemplateResponse::from).collect(),
        total,
    };

    Ok(Json(response))
}

/// Get a specific template by slug
///
/// Returns detailed information about a single template.
#[utoipa::path(
    get,
    path = "/templates/{slug}",
    tag = "Templates",
    params(
        ("slug" = String, Path, description = "Template slug")
    ),
    responses(
        (status = 200, description = "Template details", body = TemplateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Template not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_template(
    State(state): State<Arc<TemplateAppState>>,
    RequireAuth(_auth): RequireAuth,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let template = state
        .template_service
        .get_template(&slug)
        .await
        .map_err(|e| {
            problemdetails::new(http::StatusCode::NOT_FOUND)
                .with_title("Template Not Found")
                .with_detail(e.to_string())
        })?;

    Ok(Json(TemplateResponse::from(template)))
}

/// List all available tags
///
/// Returns a list of all unique tags used by public templates.
#[utoipa::path(
    get,
    path = "/templates/tags",
    tag = "Templates",
    responses(
        (status = 200, description = "List of tags", body = ListTagsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_tags(
    State(state): State<Arc<TemplateAppState>>,
    RequireAuth(_auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    let tags = state.template_service.list_tags().await;
    let total = tags.len();

    Ok(Json(ListTagsResponse { tags, total }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::templates::{GitRef, TemplatesConfig};

    fn create_test_template() -> ProjectTemplate {
        ProjectTemplate {
            slug: "test-template".to_string(),
            name: "Test Template".to_string(),
            description: Some("A test template".to_string()),
            image_url: Some("/templates/test.png".to_string()),
            git: GitRef {
                url: "https://github.com/test/test-repo.git".to_string(),
                path: None,
                r#ref: "main".to_string(),
            },
            preset: "nextjs".to_string(),
            preset_config: None,
            tags: vec!["test".to_string(), "example".to_string()],
            features: vec!["Feature 1".to_string(), "Feature 2".to_string()],
            services: vec!["postgres".to_string()],
            env_vars: vec![EnvVarTemplate {
                name: "TEST_VAR".to_string(),
                example: Some("test_value".to_string()),
                default: None,
                description: Some("A test variable".to_string()),
                required: true,
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
        };

        let response = EnvVarTemplateResponse::from(env_var);

        assert_eq!(response.name, "DATABASE_URL");
        assert_eq!(
            response.example,
            Some("postgres://localhost/db".to_string())
        );
        assert_eq!(
            response.default,
            Some("postgres://localhost/default".to_string())
        );
        assert_eq!(
            response.description,
            Some("Database connection URL".to_string())
        );
        assert!(response.required);
    }

    #[tokio::test]
    async fn test_template_service_integration() {
        let service = TemplateService::new(None);

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

        // Test list_featured_templates
        let featured = service.list_featured_templates().await;
        assert_eq!(featured.len(), 1);
        assert_eq!(featured[0].slug, "test-1");

        // Test list_templates_by_tag
        let python_templates = service.list_templates_by_tag("python").await;
        assert_eq!(python_templates.len(), 1);
        assert_eq!(python_templates[0].slug, "test-2");

        // Test get_template
        let template = service.get_template("test-1").await.unwrap();
        assert_eq!(template.name, "Test Template 1");

        // Test list_tags
        let tags = service.list_tags().await;
        assert!(tags.contains(&"fullstack".to_string()));
        assert!(tags.contains(&"python".to_string()));
    }
}
