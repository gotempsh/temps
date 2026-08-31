// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{OpenApi, ToSchema};

use super::AppState;
use crate::services::service_templates::{
    preflight_template, prepare_template, CatalogTemplateAnalysis, CoolifyTemplate,
    PreparedServiceTemplate, ServiceTemplateCatalogError, TemplateBackingService,
    TemplateCapabilityRequirement, TemplateRoute, TemplateTransformation, TemplateVariable,
    COOLIFY_CATALOG_URL, COOLIFY_REPOSITORY_URL,
};

const DEFAULT_PER_PAGE: usize = 24;
const MAX_PER_PAGE: usize = 100;
const MAX_POPULAR_TAGS: usize = 16;
const MAX_TEMPLATE_TAGS: usize = 16;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListServiceTemplatesQuery {
    /// Case-insensitive search across name, description, category, and tags.
    pub search: Option<String>,
    /// Exact case-insensitive category filter.
    pub category: Option<String>,
    /// Exact normalized discovery-tag filter.
    pub tag: Option<String>,
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
    pub backing_services: Vec<ServiceTemplateBackingServiceResponse>,
    pub port: Option<u16>,
    pub service_count: usize,
    pub installable: bool,
    /// `standard`, `elevated`, `host_access`, or `blocked`.
    pub compatibility_tier: String,
    pub compatibility_issues: Vec<String>,
    pub warnings: Vec<String>,
    pub amd_only: bool,
    pub arm_only: bool,
    /// Upstream timestamp as supplied by Coolify.
    pub template_last_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTemplateBackingServiceResponse {
    /// Compose service name that provides this dependency.
    pub service: String,
    /// Temps service family: `postgres`, `redis`, `mongodb`, or `s3`.
    pub kind: String,
    /// Bundled services remain in this Compose snapshot. Safe managed-service
    /// replacement requires a template adapter that rewrites its connection contract.
    pub mode: String,
}

impl From<TemplateBackingService> for ServiceTemplateBackingServiceResponse {
    fn from(backing_service: TemplateBackingService) -> Self {
        Self {
            service: backing_service.service,
            kind: backing_service.kind.as_str().to_string(),
            mode: "bundled".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTemplateDiscoveryTagResponse {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListServiceTemplatesResponse {
    pub templates: Vec<ServiceTemplateSummaryResponse>,
    pub categories: Vec<String>,
    /// Most common normalized tags across the complete catalog.
    pub popular_tags: Vec<ServiceTemplateDiscoveryTagResponse>,
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
    /// Blocked templates that require administrator-level host authority.
    /// This is a subset of `blocked`.
    pub host_access: usize,
    /// All non-installable templates, including `host_access` entries.
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
        let is_secret = variable.is_secret();
        Self {
            name: variable.name,
            kind: variable.kind.as_str().to_string(),
            required: variable.required,
            is_secret,
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
    /// HTTP path detected from this service's Compose healthcheck.
    pub health_check_path: Option<String>,
}

impl From<TemplateRoute> for ServiceTemplateRouteResponse {
    fn from(route: TemplateRoute) -> Self {
        Self {
            service: route.service,
            port: route.port,
            variable_names: route.variable_names,
            health_check_path: route.health_check_path,
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
    /// SHA-256 of the exact normalized Compose and deployment-critical route metadata.
    pub install_plan_digest: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PreflightServiceTemplateRequest {
    /// Project name used to plan the exact slug and canonical route hostnames.
    pub project_name: String,
    /// Digest returned by the detail endpoint; prevents validating a newer install plan.
    pub expected_install_plan_digest: String,
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
    /// Optimistically allocated slug that must be claimed with `expected_slug`.
    pub planned_project_slug: String,
    /// Canonical URL/FQDN variables calculated from the allocated slug and hostname strategy.
    pub public_variables: BTreeMap<String, String>,
}

#[derive(OpenApi)]
#[openapi(
    paths(list_service_templates, get_service_template, preflight_service_template),
    components(schemas(
        ListServiceTemplatesQuery,
        ServiceTemplateSummaryResponse,
        ServiceTemplateBackingServiceResponse,
        ServiceTemplateDiscoveryTagResponse,
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
        ("tag" = Option<String>, Query, description = "Filter by exact normalized discovery tag"),
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
    require_service_template_access(&auth)?;
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
    let tag = query.tag.as_deref().and_then(normalize_discovery_tag);
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
    let popular_tags = popular_discovery_tags(&snapshot.templates, &snapshot.analyses);

    let mut compatibility = ServiceTemplateCompatibilitySummaryResponse::default();
    for analysis in snapshot.analyses.values() {
        match analysis.compatibility_tier.as_str() {
            "standard" => compatibility.standard += 1,
            "elevated" => compatibility.elevated += 1,
            "host_access" => {
                compatibility.host_access += 1;
                compatibility.blocked += 1;
            }
            _ => compatibility.blocked += 1,
        }
    }

    let mut filtered = snapshot
        .templates
        .iter()
        .filter(|(slug, template)| {
            template_matches(
                slug,
                template,
                snapshot.analyses.get(slug.as_str()),
                search.as_deref(),
                category.as_deref(),
                tag.as_deref(),
            )
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
        .map(|(slug, template)| {
            summary_response(slug, template, snapshot.analyses.get(slug.as_str()))
        })
        .collect();

    Ok(Json(ListServiceTemplatesResponse {
        templates,
        categories,
        popular_tags,
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
    require_service_template_access(&auth)?;
    let (template, snapshot) = state
        .service_template_catalog
        .get(&slug)
        .await
        .map_err(catalog_problem)?;
    let prepared = prepare_template(&slug, &template).map_err(catalog_problem)?;
    let summary = prepared_summary_response(&slug, &template, &prepared);
    let install_plan_digest = prepared.install_plan_digest(&template);
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
        install_plan_digest,
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
        (status = 400, description = "Invalid project name"),
        (status = 409, description = "Catalog install plan changed; reload required"),
        (status = 502, description = "Upstream catalog unavailable"),
        (status = 503, description = "Docker preflight unavailable or at capacity")
    ),
    security(("bearer_auth" = []))
)]
pub async fn preflight_service_template(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(slug): Path<String>,
    Json(request): Json<PreflightServiceTemplateRequest>,
) -> Result<Json<PreflightServiceTemplateResponse>, Problem> {
    require_service_template_access(&auth)?;
    let (template, _) = state
        .service_template_catalog
        .get(&slug)
        .await
        .map_err(catalog_problem)?;
    let prepared = prepare_template(&slug, &template).map_err(catalog_problem)?;
    if prepared.install_plan_digest(&template) != request.expected_install_plan_digest {
        return Err(catalog_problem(
            ServiceTemplateCatalogError::RevisionChanged { slug },
        ));
    }
    let planned_project_slug = state
        .project_service
        .plan_project_slug(&request.project_name)
        .await
        .map_err(Problem::from)?;
    let public_variables = canonical_public_variables(&state, &planned_project_slug, &prepared)
        .await
        .map_err(catalog_problem)?;
    let mut final_values = request.variables;
    final_values.extend(public_variables.clone());
    let result = preflight_template(
        &slug,
        &template,
        &final_values,
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
        planned_project_slug,
        public_variables,
    }))
}

async fn canonical_public_variables(
    state: &AppState,
    project_slug: &str,
    prepared: &PreparedServiceTemplate,
) -> Result<BTreeMap<String, String>, ServiceTemplateCatalogError> {
    let settings = state.config_service.get_settings().await.map_err(|error| {
        ServiceTemplateCatalogError::PreflightInfrastructure {
            slug: project_slug.to_string(),
            reason: format!("could not load hostname settings: {error}"),
        }
    })?;
    let preview_domain = if settings.preview_domain.trim().is_empty() {
        "localho.st"
    } else {
        settings.preview_domain.trim()
    };
    let strategy = state
        .public_hostname_resolver
        .strategy_for(preview_domain)
        .await;
    let (scheme, port) = settings
        .external_url
        .as_deref()
        .and_then(|value| url::Url::parse(value).ok())
        .map(|url| (url.scheme().to_string(), url.port()))
        .unwrap_or_else(|| ("http".to_string(), Some(state.config_service.proxy_port())));
    let environment_slug = format!("{project_slug}-production");
    Ok(build_canonical_public_variables(
        prepared,
        &environment_slug,
        preview_domain,
        strategy,
        &scheme,
        port,
    ))
}

fn build_canonical_public_variables(
    prepared: &PreparedServiceTemplate,
    environment_slug: &str,
    preview_domain: &str,
    strategy: temps_core::PublicHostnameStrategy,
    scheme: &str,
    port: Option<u16>,
) -> BTreeMap<String, String> {
    let primary_service = prepared.routes.first().map(|route| route.service.as_str());
    let mut values = BTreeMap::new();
    for variable in &prepared.variables {
        if !matches!(variable.kind.as_str(), "public_url" | "public_host") {
            continue;
        }
        let route_service = variable
            .route_service
            .as_deref()
            .or(primary_service)
            .unwrap_or("app");
        let hostname = if Some(route_service) == primary_service {
            strategy.environment_hostname(preview_domain, environment_slug)
        } else {
            strategy.service_hostname(preview_domain, environment_slug, route_service)
        };
        let port_suffix = port
            .filter(|port| {
                !((scheme == "http" && *port == 80) || (scheme == "https" && *port == 443))
            })
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let path = variable
            .default_value
            .as_deref()
            .filter(|value| value.starts_with('/'))
            .unwrap_or_default();
        let value = if variable.kind.as_str() == "public_url" {
            format!("{scheme}://{hostname}{port_suffix}{path}")
        } else {
            format!("{hostname}{path}")
        };
        values.insert(variable.name.clone(), value);
    }
    values
}

fn summary_response(
    slug: &str,
    template: &CoolifyTemplate,
    analysis: Option<&CatalogTemplateAnalysis>,
) -> ServiceTemplateSummaryResponse {
    match analysis {
        Some(analysis) => ServiceTemplateSummaryResponse {
            slug: slug.to_string(),
            name: display_name(slug),
            description: template.slogan.clone(),
            documentation_url: template.documentation.as_deref().and_then(safe_http_url),
            logo_url: template.logo.as_deref().and_then(logo_url),
            category: template_category(template),
            tags: template_discovery_tags(template, &analysis.backing_services),
            backing_services: analysis
                .backing_services
                .clone()
                .into_iter()
                .map(Into::into)
                .collect(),
            port: template.port.as_deref().and_then(|port| port.parse().ok()),
            service_count: analysis.service_count,
            installable: analysis.installable,
            compatibility_tier: analysis.compatibility_tier.as_str().to_string(),
            compatibility_issues: analysis.compatibility_issues.clone(),
            warnings: analysis.warnings.clone(),
            amd_only: template.amd_only,
            arm_only: template.arm_only,
            template_last_updated_at: template.template_last_updated_at.clone(),
        },
        None => ServiceTemplateSummaryResponse {
            slug: slug.to_string(),
            name: display_name(slug),
            description: template.slogan.clone(),
            documentation_url: template.documentation.as_deref().and_then(safe_http_url),
            logo_url: template.logo.as_deref().and_then(logo_url),
            category: template_category(template),
            tags: template_discovery_tags(template, &[]),
            backing_services: Vec::new(),
            port: template.port.as_deref().and_then(|port| port.parse().ok()),
            service_count: 0,
            installable: false,
            compatibility_tier: "blocked".to_string(),
            compatibility_issues: vec!["Catalog analysis is unavailable".to_string()],
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
        tags: template_discovery_tags(template, &prepared.backing_services),
        backing_services: prepared
            .backing_services
            .clone()
            .into_iter()
            .map(Into::into)
            .collect(),
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

fn template_matches(
    slug: &str,
    template: &CoolifyTemplate,
    analysis: Option<&CatalogTemplateAnalysis>,
    search: Option<&str>,
    category: Option<&str>,
    tag: Option<&str>,
) -> bool {
    let discovery_tags = template_discovery_tags(
        template,
        analysis
            .map(|analysis| analysis.backing_services.as_slice())
            .unwrap_or_default(),
    );
    category.is_none_or(|category| template_category(template).to_ascii_lowercase() == category)
        && tag.is_none_or(|tag| discovery_tags.iter().any(|candidate| candidate == tag))
        && search.is_none_or(|search| {
            let mut haystack = format!(
                "{} {} {} {}",
                slug,
                template.slogan.as_deref().unwrap_or_default(),
                template_category(template),
                discovery_tags.join(" ")
            );
            haystack.make_ascii_lowercase();
            haystack.contains(search)
        })
}

fn template_discovery_tags(
    template: &CoolifyTemplate,
    backing_services: &[TemplateBackingService],
) -> Vec<String> {
    let mut tags = BTreeSet::new();
    for category in template_category(template).split([',', '/', '&']) {
        if let Some(tag) = normalize_discovery_tag(category) {
            tags.insert(tag);
        }
    }
    for upstream_tag in &template.tags {
        if let Some(tag) = normalize_discovery_tag(upstream_tag) {
            tags.insert(tag);
        }
    }
    for backing_service in backing_services {
        tags.insert(backing_service.kind.discovery_tag().to_string());
    }
    tags.into_iter().take(MAX_TEMPLATE_TAGS).collect()
}

fn normalize_discovery_tag(value: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut separator_pending = false;
    for character in value.trim().to_lowercase().chars() {
        if character.is_alphanumeric() {
            if separator_pending && !normalized.is_empty() {
                normalized.push('-');
            }
            normalized.push(character);
            separator_pending = false;
        } else if !normalized.is_empty() {
            separator_pending = true;
        }
    }
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty() || normalized.len() > 40 {
        return None;
    }
    let canonical = match normalized {
        "opensource" | "open-source" => "open-source",
        "nocode" | "no-code" => "no-code",
        "machinelearning" | "machine-learning" => "machine-learning",
        "versioncontrol" | "version-control" => "version-control",
        "devtools" | "dev-tools" | "developer-tools" | "development-tools" => "developer-tools",
        "postgres" | "postgresql" => "postgresql",
        "mongo" | "mongo-db" | "mongodb" => "mongodb",
        "object-storage" | "s3-storage" | "minio" => "s3",
        "auth" => "authentication",
        "open" | "source" | "self-hosted" | "server" | "web" | "application" | "applications"
        | "platform" | "low" => return None,
        value => value,
    };
    Some(canonical.to_string())
}

fn popular_discovery_tags(
    templates: &BTreeMap<String, CoolifyTemplate>,
    analyses: &BTreeMap<String, CatalogTemplateAnalysis>,
) -> Vec<ServiceTemplateDiscoveryTagResponse> {
    let mut counts = BTreeMap::<String, usize>::new();
    for (slug, template) in templates {
        let backing_services = analyses
            .get(slug)
            .map(|analysis| analysis.backing_services.as_slice())
            .unwrap_or_default();
        for tag in template_discovery_tags(template, backing_services) {
            *counts.entry(tag).or_default() += 1;
        }
    }
    let mut tags = counts
        .into_iter()
        .map(|(name, count)| ServiceTemplateDiscoveryTagResponse { name, count })
        .collect::<Vec<_>>();
    tags.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    tags.truncate(MAX_POPULAR_TAGS);
    tags
}

fn require_service_template_access(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, ProjectsCreate);
    permission_guard!(auth, DeploymentsCreate);
    Ok(())
}

fn safe_http_url(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let query = url
        .query_pairs()
        .map(|(name, value)| {
            if name.eq_ignore_ascii_case("utm_source") {
                (name.into_owned(), "temps.sh".to_string())
            } else {
                (name.into_owned(), value.into_owned())
            }
        })
        .collect::<Vec<_>>();
    if url.query().is_some() {
        url.query_pairs_mut().clear().extend_pairs(query);
    }
    Some(url.to_string())
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
        ServiceTemplateCatalogError::RevisionChanged { .. } => {
            problemdetails::new(StatusCode::CONFLICT)
                .with_title("Service Template Changed")
                .with_detail(error.to_string())
        }
        ServiceTemplateCatalogError::PreflightInfrastructure { .. } => {
            problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Service Template Preflight Unavailable")
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
    use axum::response::IntoResponse;
    use base64::Engine;
    use chrono::Utc;
    use temps_auth::Permission;
    use temps_entities::users;

    fn api_key_auth(permissions: Vec<Permission>) -> AuthContext {
        let now = Utc::now();
        let user = users::Model {
            id: 42,
            name: "Template Tester".to_string(),
            email: "templates@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_api_key(
            user,
            None,
            Some(permissions),
            "templates-test".to_string(),
            1,
        )
    }

    fn prepared(compose: &str) -> PreparedServiceTemplate {
        prepare_template(
            "routes",
            &CoolifyTemplate {
                documentation: None,
                slogan: None,
                compose: base64::engine::general_purpose::STANDARD.encode(compose),
                tags: Vec::new(),
                category: None,
                logo: None,
                port: None,
                template_last_updated_at: None,
                amd_only: false,
                arm_only: false,
            },
        )
        .unwrap()
    }

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
        assert_eq!(
            safe_http_url("https://www.keycloak.org/?utm_source=coolify.io&utm_medium=referral"),
            Some("https://www.keycloak.org/?utm_source=temps.sh&utm_medium=referral".to_string())
        );
    }

    #[test]
    fn catalog_access_requires_project_and_deployment_create_permissions() {
        let denied = require_service_template_access(&api_key_auth(Vec::new()))
            .expect_err("custom key should be denied")
            .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let projects_only =
            require_service_template_access(&api_key_auth(vec![Permission::ProjectsCreate]))
                .expect_err("project-only key cannot complete the advertised install flow")
                .into_response();
        assert_eq!(projects_only.status(), StatusCode::FORBIDDEN);

        let deployments_only =
            require_service_template_access(&api_key_auth(vec![Permission::DeploymentsCreate]))
                .expect_err("deployment-only key cannot create the template project")
                .into_response();
        assert_eq!(deployments_only.status(), StatusCode::FORBIDDEN);

        let allowed = api_key_auth(vec![
            Permission::ProjectsCreate,
            Permission::DeploymentsCreate,
        ]);
        assert!(require_service_template_access(&allowed).is_ok());
    }

    #[test]
    fn catalog_filter_matches_search_tags_and_exact_category() {
        let template = CoolifyTemplate {
            documentation: None,
            slogan: Some("Private budgeting".to_string()),
            compose: String::new(),
            tags: vec!["finance".to_string(), "money".to_string()],
            category: Some("Productivity".to_string()),
            logo: None,
            port: None,
            template_last_updated_at: None,
            amd_only: false,
            arm_only: false,
        };

        assert!(template_matches(
            "actualbudget",
            &template,
            None,
            Some("finance"),
            Some("productivity"),
            None,
        ));
        assert!(template_matches(
            "actualbudget",
            &template,
            None,
            Some("budget"),
            None,
            None,
        ));
        assert!(!template_matches(
            "actualbudget",
            &template,
            None,
            None,
            Some("database"),
            None,
        ));
        assert!(template_matches(
            "actualbudget",
            &template,
            None,
            None,
            None,
            Some("finance"),
        ));
        assert!(!template_matches(
            "actualbudget",
            &template,
            None,
            None,
            None,
            Some("fin"),
        ));
    }

    #[test]
    fn canonical_public_values_use_backend_hostname_strategies() {
        let prepared = prepared(
            r#"services:
  app:
    image: example/app:1
    environment:
      - SERVICE_URL_APP_3000=/console
  admin_api:
    image: example/admin:1
    environment:
      - SERVICE_FQDN_ADMIN_3001
"#,
        );

        let standard = build_canonical_public_variables(
            &prepared,
            "example-production",
            "apps.example.com",
            temps_core::PublicHostnameStrategy::Standard,
            "https",
            None,
        );
        assert_eq!(
            standard["SERVICE_URL_APP_3000"],
            "https://example-production.apps.example.com/console"
        );
        assert_eq!(
            standard["SERVICE_FQDN_ADMIN_3001"],
            "admin-api--example-production.apps.example.com"
        );

        let flat = build_canonical_public_variables(
            &prepared,
            "example-production",
            "apps.example.com",
            temps_core::PublicHostnameStrategy::Flat,
            "http",
            Some(8080),
        );
        assert_eq!(
            flat["SERVICE_FQDN_ADMIN_3001"],
            "example-production--admin-api.apps.example.com"
        );
        assert_eq!(
            flat["SERVICE_URL_APP_3000"],
            "http://example-production.apps.example.com:8080/console"
        );
    }

    #[test]
    fn revision_changes_are_reported_as_conflicts() {
        let response = catalog_problem(ServiceTemplateCatalogError::RevisionChanged {
            slug: "example".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn preflight_infrastructure_failures_are_service_unavailable() {
        let response = catalog_problem(ServiceTemplateCatalogError::PreflightInfrastructure {
            slug: "example".to_string(),
            reason: "Docker is unavailable".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn missing_and_invalid_templates_have_typed_http_statuses() {
        let missing = catalog_problem(ServiceTemplateCatalogError::NotFound {
            slug: "missing".to_string(),
        })
        .into_response();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let invalid = catalog_problem(ServiceTemplateCatalogError::InvalidPreflightInput {
            slug: "example".to_string(),
            reason: "undeclared variable".to_string(),
        })
        .into_response();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn variable_response_uses_backend_secret_classification() {
        let response = ServiceTemplateVariableResponse::from(TemplateVariable {
            name: "PUSH_SERVICE_KEY".to_string(),
            kind: crate::services::service_templates::TemplateVariableKind::UserInput,
            required: true,
            default_value: None,
            route_service: None,
        });
        assert!(response.is_secret);
    }
}
