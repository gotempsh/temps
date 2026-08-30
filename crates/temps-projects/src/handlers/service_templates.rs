// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{OpenApi, ToSchema};

use super::AppState;
use crate::services::service_templates::{
    preflight_template, prepare_template, CoolifyTemplate, PreparedServiceTemplate,
    ServiceTemplateCatalogError, TemplateCapabilityRequirement, TemplateRoute,
    TemplateTransformation, TemplateVariable, COOLIFY_CATALOG_URL, COOLIFY_REPOSITORY_URL,
};

const DEFAULT_PER_PAGE: usize = 24;
const MAX_PER_PAGE: usize = 100;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListServiceTemplatesQuery {
    /// Case-insensitive search across name, description, category, and tags.
    pub search: Option<String>,
    /// Exact case-insensitive category filter.
    pub category: Option<String>,
    /// One-based page number. Defaults to 1.
    pub page: Option<usize>,
    /// Results per page. Defaults to 24 and is capped at 100.
    pub per_page: Option<usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTemplateSummaryResponse {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub documentation_url: Option<String>,
    pub logo_url: Option<String>,
    pub category: String,
    pub tags: Vec<String>,
    pub port: Option<u16>,
    pub service_count: usize,
    pub installable: bool,
    /// `standard`, `elevated`, or `blocked`.
    pub compatibility_tier: String,
    pub compatibility_issues: Vec<String>,
    pub warnings: Vec<String>,
    pub amd_only: bool,
    pub arm_only: bool,
    /// Upstream timestamp as supplied by Coolify.
    pub template_last_updated_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListServiceTemplatesResponse {
    pub templates: Vec<ServiceTemplateSummaryResponse>,
    pub categories: Vec<String>,
    /// Total entries in the upstream catalog before filters are applied.
    pub catalog_total: usize,
    /// Total entries matching the current search and category filters.
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
    pub total_pages: usize,
    pub catalog_fetched_at: String,
    pub source_url: String,
    pub source_repository_url: String,
    pub source_revision: Option<String>,
    pub compatibility: ServiceTemplateCompatibilitySummaryResponse,
}

#[derive(Debug, Default, Serialize, ToSchema)]
pub struct ServiceTemplateCompatibilitySummaryResponse {
    pub standard: usize,
    pub elevated: usize,
    pub blocked: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceTemplateVariableResponse {
    pub name: String,
    /// Generator/input type used by the installer.
    pub kind: String,
    pub required: bool,
    pub is_secret: bool,
    pub default_value: Option<String>,
    pub route_service: Option<String>,
}

impl From<TemplateVariable> for ServiceTemplateVariableResponse {
    fn from(variable: TemplateVariable) -> Self {
        Self {
            name: variable.name,
            kind: variable.kind.as_str().to_string(),
            required: variable.required,
            is_secret: variable.kind.is_secret(),
            default_value: variable.default_value,
            route_service: variable.route_service,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceTemplateRouteResponse {
    pub service: String,
    pub port: u16,
    pub variable_names: Vec<String>,
}

impl From<TemplateRoute> for ServiceTemplateRouteResponse {
    fn from(route: TemplateRoute) -> Self {
        Self {
            service: route.service,
            port: route.port,
            variable_names: route.variable_names,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceTemplateTransformationResponse {
    pub code: String,
    pub description: String,
}

impl From<TemplateTransformation> for ServiceTemplateTransformationResponse {
    fn from(transformation: TemplateTransformation) -> Self {
        Self {
            code: transformation.code.to_string(),
            description: transformation.description,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceTemplateCapabilityRequirementResponse {
    pub service: String,
    pub capability: String,
    pub reason: String,
}

impl From<TemplateCapabilityRequirement> for ServiceTemplateCapabilityRequirementResponse {
    fn from(requirement: TemplateCapabilityRequirement) -> Self {
        Self {
            service: requirement.service,
            capability: requirement.capability.to_string(),
            reason: requirement.reason,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceTemplateDetailResponse {
    #[serde(flatten)]
    pub summary: ServiceTemplateSummaryResponse,
    /// Normalized Compose copied into the new project when installed.
    pub compose: String,
    pub routes: Vec<ServiceTemplateRouteResponse>,
    pub variables: Vec<ServiceTemplateVariableResponse>,
    pub transformations: Vec<ServiceTemplateTransformationResponse>,
    pub capability_requirements: Vec<ServiceTemplateCapabilityRequirementResponse>,
    pub catalog_fetched_at: String,
    pub source_url: String,
    pub source_repository_url: String,
    pub source_revision: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PreflightServiceTemplateRequest {
    /// Final environment values the project would persist. Values are never returned.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Services for which the user explicitly approved limited startup capabilities.
    #[serde(default)]
    pub approved_capability_services: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreflightServiceTemplateResponse {
    pub ready: bool,
    pub compose_validated: bool,
    pub architecture: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(OpenApi)]
#[openapi(
    paths(list_service_templates, get_service_template, preflight_service_template),
    components(schemas(
        ListServiceTemplatesQuery,
        ServiceTemplateSummaryResponse,
        ListServiceTemplatesResponse,
        ServiceTemplateVariableResponse,
        ServiceTemplateDetailResponse,
        ServiceTemplateRouteResponse,
        ServiceTemplateTransformationResponse,
        ServiceTemplateCapabilityRequirementResponse,
        ServiceTemplateCompatibilitySummaryResponse,
        PreflightServiceTemplateRequest,
        PreflightServiceTemplateResponse,
    )),
    tags((name = "Service Templates", description = "Runtime-synced Docker Compose service catalog"))
)]
pub struct ServiceTemplatesApiDoc;

/// Browse Coolify's one-click Docker Compose catalog.
#[utoipa::path(
    get,
    path = "/",
    tag = "Service Templates",
    operation_id = "list_service_templates",
    params(
        ("search" = Option<String>, Query, description = "Search name, description, category, and tags"),
        ("category" = Option<String>, Query, description = "Filter by category"),
        ("page" = Option<usize>, Query, description = "One-based page number"),
        ("per_page" = Option<usize>, Query, description = "Results per page, maximum 100")
    ),
    responses(
        (status = 200, description = "Paginated service template catalog", body = ListServiceTemplatesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 502, description = "Upstream catalog unavailable")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_service_templates(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<ListServiceTemplatesQuery>,
) -> Result<Json<ListServiceTemplatesResponse>, Problem> {
    permission_guard!(auth, ProjectsCreate);
    let snapshot = state
        .service_template_catalog
        .snapshot()
        .await
        .map_err(catalog_problem)?;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let category = query
        .category
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query
        .per_page
        .unwrap_or(DEFAULT_PER_PAGE)
        .clamp(1, MAX_PER_PAGE);

    let mut categories = snapshot
        .templates
        .values()
        .map(template_category)
        .collect::<Vec<_>>();
    categories.sort_by_key(|value| value.to_ascii_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let mut compatibility = ServiceTemplateCompatibilitySummaryResponse::default();
    for (slug, template) in &snapshot.templates {
        match prepare_template(slug, template) {
            Ok(prepared) => match prepared.compatibility_tier().as_str() {
                "standard" => compatibility.standard += 1,
                "elevated" => compatibility.elevated += 1,
                _ => compatibility.blocked += 1,
            },
            Err(_) => compatibility.blocked += 1,
        }
    }

    let mut filtered = snapshot
        .templates
        .iter()
        .filter(|(slug, template)| {
            category.as_ref().is_none_or(|category| {
                template_category(template).to_ascii_lowercase() == *category
            }) && search.as_ref().is_none_or(|search| {
                let mut haystack = format!(
                    "{} {} {} {}",
                    slug,
                    template.slogan.as_deref().unwrap_or_default(),
                    template_category(template),
                    template.tags.join(" ")
                );
                haystack.make_ascii_lowercase();
                haystack.contains(search)
            })
        })
        .collect::<Vec<_>>();
    filtered.sort_by(|(left_slug, _), (right_slug, _)| {
        display_name(left_slug).cmp(&display_name(right_slug))
    });
    let total = filtered.len();
    let total_pages = total.div_ceil(per_page);
    let offset = page.saturating_sub(1).saturating_mul(per_page);
    let templates = filtered
        .into_iter()
        .skip(offset)
        .take(per_page)
        .map(|(slug, template)| summary_response(slug, template))
        .collect();

    Ok(Json(ListServiceTemplatesResponse {
        templates,
        categories,
        catalog_total: snapshot.templates.len(),
        total,
        page,
        per_page,
        total_pages,
        catalog_fetched_at: snapshot
            .fetched_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        source_url: COOLIFY_CATALOG_URL.to_string(),
        source_repository_url: COOLIFY_REPOSITORY_URL.to_string(),
        source_revision: snapshot.etag.clone(),
        compatibility,
    }))
}

/// Inspect a normalized service template before installing it.
#[utoipa::path(
    get,
    path = "/{slug}",
    tag = "Service Templates",
    operation_id = "get_service_template",
    params(("slug" = String, Path, description = "Coolify template slug")),
    responses(
        (status = 200, description = "Service template detail and compatibility analysis", body = ServiceTemplateDetailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Template not found"),
        (status = 422, description = "Template content is invalid"),
        (status = 502, description = "Upstream catalog unavailable")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_service_template(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(slug): Path<String>,
) -> Result<Json<ServiceTemplateDetailResponse>, Problem> {
    permission_guard!(auth, ProjectsCreate);
    let (template, snapshot) = state
        .service_template_catalog
        .get(&slug)
        .await
        .map_err(catalog_problem)?;
    let prepared = prepare_template(&slug, &template).map_err(catalog_problem)?;
    let summary = prepared_summary_response(&slug, &template, &prepared);
    Ok(Json(ServiceTemplateDetailResponse {
        summary,
        compose: prepared.compose,
        routes: prepared
            .routes
            .into_iter()
            .map(ServiceTemplateRouteResponse::from)
            .collect(),
        variables: prepared
            .variables
            .into_iter()
            .map(ServiceTemplateVariableResponse::from)
            .collect(),
        transformations: prepared
            .transformations
            .into_iter()
            .map(ServiceTemplateTransformationResponse::from)
            .collect(),
        capability_requirements: prepared
            .capability_requirements
            .into_iter()
            .map(ServiceTemplateCapabilityRequirementResponse::from)
            .collect(),
        catalog_fetched_at: snapshot
            .fetched_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        source_url: COOLIFY_CATALOG_URL.to_string(),
        source_repository_url: COOLIFY_REPOSITORY_URL.to_string(),
        source_revision: snapshot.etag.clone(),
    }))
}

/// Validate a fully configured template without creating a project or containers.
#[utoipa::path(
    post,
    path = "/{slug}/preflight",
    tag = "Service Templates",
    operation_id = "preflight_service_template",
    params(("slug" = String, Path, description = "Coolify template slug")),
    request_body = PreflightServiceTemplateRequest,
    responses(
        (status = 200, description = "Template preflight result", body = PreflightServiceTemplateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Template not found"),
        (status = 422, description = "Template content is invalid"),
        (status = 502, description = "Upstream catalog unavailable")
    ),
    security(("bearer_auth" = []))
)]
pub async fn preflight_service_template(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(slug): Path<String>,
    Json(request): Json<PreflightServiceTemplateRequest>,
) -> Result<Json<PreflightServiceTemplateResponse>, Problem> {
    permission_guard!(auth, ProjectsCreate);
    let (template, _) = state
        .service_template_catalog
        .get(&slug)
        .await
        .map_err(catalog_problem)?;
    let result = preflight_template(
        &slug,
        &template,
        &request.variables,
        &request.approved_capability_services,
    )
    .await
    .map_err(catalog_problem)?;
    Ok(Json(PreflightServiceTemplateResponse {
        ready: result.ready(),
        compose_validated: result.compose_validated,
        architecture: result.architecture,
        errors: result.errors,
        warnings: result.warnings,
    }))
}

fn summary_response(slug: &str, template: &CoolifyTemplate) -> ServiceTemplateSummaryResponse {
    match prepare_template(slug, template) {
        Ok(prepared) => prepared_summary_response(slug, template, &prepared),
        Err(error) => ServiceTemplateSummaryResponse {
            slug: slug.to_string(),
            name: display_name(slug),
            description: template.slogan.clone(),
            documentation_url: template.documentation.as_deref().and_then(safe_http_url),
            logo_url: template.logo.as_deref().and_then(logo_url),
            category: template_category(template),
            tags: template.tags.clone(),
            port: template.port.as_deref().and_then(|port| port.parse().ok()),
            service_count: 0,
            installable: false,
            compatibility_tier: "blocked".to_string(),
            compatibility_issues: vec![error.to_string()],
            warnings: Vec::new(),
            amd_only: template.amd_only,
            arm_only: template.arm_only,
            template_last_updated_at: template.template_last_updated_at.clone(),
        },
    }
}

fn prepared_summary_response(
    slug: &str,
    template: &CoolifyTemplate,
    prepared: &PreparedServiceTemplate,
) -> ServiceTemplateSummaryResponse {
    ServiceTemplateSummaryResponse {
        slug: slug.to_string(),
        name: display_name(slug),
        description: template.slogan.clone(),
        documentation_url: template.documentation.as_deref().and_then(safe_http_url),
        logo_url: template.logo.as_deref().and_then(logo_url),
        category: template_category(template),
        tags: template.tags.clone(),
        port: prepared.routes.first().map(|route| route.port),
        service_count: prepared.service_count,
        installable: prepared.installable(),
        compatibility_tier: prepared.compatibility_tier().as_str().to_string(),
        compatibility_issues: prepared.compatibility_issues.clone(),
        warnings: prepared.warnings.clone(),
        amd_only: template.amd_only,
        arm_only: template.arm_only,
        template_last_updated_at: template.template_last_updated_at.clone(),
    }
}

fn display_name(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn template_category(template: &CoolifyTemplate) -> String {
    template
        .category
        .as_deref()
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .unwrap_or("Other")
        .to_string()
}

fn safe_http_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

fn logo_url(path: &str) -> Option<String> {
    let path = path.trim_start_matches('/');
    if !path.starts_with("svgs/") || path.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(format!(
        "https://raw.githubusercontent.com/coollabsio/coolify/refs/heads/main/public/{}",
        path
    ))
}

fn catalog_problem(error: ServiceTemplateCatalogError) -> Problem {
    match error {
        ServiceTemplateCatalogError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Service Template Not Found")
            .with_detail(error.to_string()),
        ServiceTemplateCatalogError::InvalidComposeEncoding { .. }
        | ServiceTemplateCatalogError::InvalidComposeText { .. }
        | ServiceTemplateCatalogError::ComposeTooLarge { .. }
        | ServiceTemplateCatalogError::InvalidComposeYaml { .. }
        | ServiceTemplateCatalogError::InvalidPreflightInput { .. } => {
            problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                .with_title("Invalid Service Template")
                .with_detail(error.to_string())
        }
        ServiceTemplateCatalogError::PreflightBusy { .. } => {
            problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Service Template Preflight Busy")
                .with_detail(error.to_string())
        }
        ServiceTemplateCatalogError::ClientBuild { .. }
        | ServiceTemplateCatalogError::Fetch { .. }
        | ServiceTemplateCatalogError::HttpStatus { .. }
        | ServiceTemplateCatalogError::CatalogTooLarge { .. }
        | ServiceTemplateCatalogError::InvalidCatalog { .. }
        | ServiceTemplateCatalogError::TooManyEntries { .. } => {
            problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("Service Template Catalog Unavailable")
                .with_detail(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_humanizes_catalog_slug() {
        assert_eq!(
            display_name("wordpress-with-mariadb"),
            "Wordpress With Mariadb"
        );
        assert_eq!(display_name("actual_budget"), "Actual Budget");
    }

    #[test]
    fn logo_url_is_confined_to_upstream_repository() {
        assert_eq!(
            logo_url("svgs/example.svg"),
            Some(
                "https://raw.githubusercontent.com/coollabsio/coolify/refs/heads/main/public/svgs/example.svg"
                    .to_string()
            )
        );
        assert_eq!(logo_url("svgs/../../etc/passwd"), None);
        assert_eq!(logo_url("other/example.svg"), None);
    }

    #[test]
    fn documentation_urls_only_allow_http_protocols() {
        assert_eq!(
            safe_http_url("https://example.com/docs"),
            Some("https://example.com/docs".to_string())
        );
        assert_eq!(safe_http_url("javascript:alert(1)"), None);
    }
}
