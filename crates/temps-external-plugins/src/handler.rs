//! HTTP handlers for external plugin management endpoints.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use temps_core::external_plugin::{NavEntry, NavSection, PluginManifest, UiManifest, UiRoute};
use utoipa::OpenApi as OpenApiTrait;

use crate::service::ExternalPluginsService;

/// Handler state for the external plugins API.
#[derive(Clone)]
pub struct ExternalPluginsAppState {
    pub service: Arc<ExternalPluginsService>,
}

/// List all running external plugins and their manifests.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins",
    responses(
        (status = 200, description = "List of all running external plugins", body = Vec<PluginManifest>),
    ),
    security(("bearer_auth" = []))
)]
async fn list_external_plugins(State(state): State<ExternalPluginsAppState>) -> impl IntoResponse {
    Json(state.service.manifests().to_vec())
}

/// Build the router for external plugin management endpoints.
pub fn configure_routes() -> Router<ExternalPluginsAppState> {
    Router::new().route("/x/plugins", get(list_external_plugins))
}

#[derive(OpenApiTrait)]
#[openapi(
    paths(list_external_plugins),
    components(
        schemas(
            PluginManifest,
            NavEntry,
            NavSection,
            UiManifest,
            UiRoute,
        )
    ),
    tags(
        (name = "External Plugins", description = "External plugin management and discovery")
    )
)]
pub struct ExternalPluginsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openapi_spec_has_plugins_path() {
        let spec = ExternalPluginsApiDoc::openapi();
        assert!(
            spec.paths.paths.contains_key("/x/plugins"),
            "OpenAPI spec must contain /x/plugins path"
        );
    }

    #[test]
    fn test_openapi_spec_has_schemas() {
        let spec = ExternalPluginsApiDoc::openapi();
        let components = spec.components.expect("should have components");
        assert!(
            components.schemas.contains_key("PluginManifest"),
            "OpenAPI spec must contain PluginManifest schema"
        );
        assert!(
            components.schemas.contains_key("NavEntry"),
            "OpenAPI spec must contain NavEntry schema"
        );
        assert!(
            components.schemas.contains_key("NavSection"),
            "OpenAPI spec must contain NavSection schema"
        );
    }
}
