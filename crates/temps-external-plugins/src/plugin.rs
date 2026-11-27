// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TempsPlugin implementation for external plugin management.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::Request;
use axum::response::IntoResponse;
use axum::Router;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::JobQueue;
use tokio::sync::RwLock;
use tower::ServiceExt;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handler::{self, ExternalPluginsApiDoc, ExternalPluginsAppState};
use crate::manager::ExternalPluginConfig;
use crate::sandbox::{ContainerSandbox, SandboxHost};
use crate::service::ExternalPluginsService;
use tracing::{debug, warn};

/// Swappable proxy router holder.
///
/// The `Arc<RwLock<Router>>` is read on every proxied request and written to
/// during [`ExternalPluginsService::reload_plugins`]. This allows hot-swapping
/// plugin proxy routes without restarting the server.
pub(crate) struct DynamicPluginRouter {
    pub inner: Arc<RwLock<Router>>,
}

impl DynamicPluginRouter {
    pub(crate) fn router(&self) -> Router {
        let router_ref = self.inner.clone();
        Router::new().fallback(move |request: Request| {
            let router_ref = router_ref.clone();
            async move {
                let router = router_ref.read().await.clone();
                router.oneshot(request).await.into_response()
            }
        })
    }
}

/// External plugins plugin — discovers, manages, and proxies standalone
/// binary plugins following the TempsPlugin lifecycle.
pub struct ExternalPluginsPlugin {
    config: ExternalPluginConfig,
    /// OpenAPI schemas collected from running external plugins during startup.
    /// Populated synchronously at the end of `register_services()` so it is
    /// available when the synchronous `openapi_schema()` trait method is called.
    cached_schemas: std::sync::Mutex<Option<Vec<(String, utoipa::openapi::OpenApi)>>>,
}

impl ExternalPluginsPlugin {
    pub fn new(config: ExternalPluginConfig) -> Self {
        Self {
            config,
            cached_schemas: std::sync::Mutex::new(None),
        }
    }
}

impl TempsPlugin for ExternalPluginsPlugin {
    fn name(&self) -> &'static str {
        "external-plugins"
    }

    fn required_services(&self) -> Vec<temps_core::plugin::RequiredService> {
        use temps_core::plugin::RequiredService;
        vec![
            RequiredService::of::<sea_orm::DatabaseConnection>(),
            RequiredService::of::<temps_core::DockerHandle>(),
        ]
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Try to get the JobQueue from the service registry (registered by queue plugin).
            // This is optional — event delivery is disabled if no queue is available.
            let queue: Option<Arc<dyn JobQueue>> = context.get_service::<dyn JobQueue>();

            // Get the database connection for the platform channel.
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create an empty service shell — does NOT discover/start plugins.
            // Discovery is deferred to a background task because the
            // per-plugin handshake timeout (default 30s) would otherwise
            // block the console API from coming up.
            // A process with no Docker of its own (a hosted control plane)
            // compiles plugins on a joined worker node. Registered by the
            // deployments plugin, which runs earlier; absent where the node
            // machinery is not part of the build.
            let remote_builder =
                context.get_service::<dyn temps_core::plugin_builder::RemotePluginBuilder>();
            let ai_tools = Arc::new(temps_ai_api_tools::PluginToolsRegistry::default());
            context.register_service(ai_tools.clone());
            // The Docker client deployments run on, registered in every
            // profile (as a typed "disabled" where there is none), and where
            // this process itself runs. An embedding that registers no
            // runtime context keeps the historical host behaviour.
            let docker = context.require_service::<temps_core::DockerHandle>();
            let inside_container = context
                .get_service::<temps_core::RuntimeContext>()
                .is_some_and(|runtime| {
                    runtime.execution_environment()
                        == temps_core::runtime::ExecutionEnvironment::Docker
                });
            let config = self.config.clone().with_sandbox(ContainerSandbox::new(
                docker.as_ref().clone(),
                SandboxHost::detect(inside_container),
            ));
            if let Err(reason) = config.sandbox.availability() {
                tracing::warn!(
                    reason = %reason,
                    "Plugins cannot run in containers on this server; a plugin set to container isolation will not start"
                );
            }
            let service = Arc::new(
                ExternalPluginsService::new_empty(config, queue, db)
                    .with_remote_builder(remote_builder)
                    .with_ai_tools_registry(ai_tools),
            );
            service
                .manager()
                .set_ai_service(context.require_service::<dyn temps_ai::AiService>())
                .await;
            service
                .manager()
                .set_audit_service(context.require_service::<dyn temps_core::AuditLogger>())
                .await;
            // What attaching to a plugin that runs as a Temps service needs:
            // the token its containers were deployed with (registered by the
            // deployments plugin, which runs earlier) and where this process
            // runs, to reach those containers. Absent in an embedding that
            // deploys nothing; adding a service as a plugin then says so.
            match (
                context.get_service::<dyn temps_core::DeploymentTokenSource>(),
                context.get_service::<temps_core::RuntimeContext>(),
            ) {
                (Some(tokens), Some(runtime)) => {
                    let mut services = crate::service_runtime::ServiceRuntime::new(
                        tokens,
                        runtime.as_ref().clone(),
                    );
                    // Starts an environment that stopped for being idle. Only
                    // a process that runs the proxy owns that lifecycle.
                    if let Some(waker) = context.get_service::<dyn temps_core::OnDemandWaker>() {
                        services = services.with_waker(waker);
                    }
                    service.manager().set_service_runtime(services).await;
                }
                _ => debug!(
                    "No deployments in this process; plugins cannot be added from services here"
                ),
            }
            // Kick off discovery + start in the background. When it
            // completes, the shared proxy router is swapped in and
            // `/x/<plugin>/...` routes start working.
            service.clone().start_background_discovery();

            // Get the swappable router reference. The router is empty now
            // and gets populated in-place once background discovery
            // finishes — same swap mechanism used by /x/plugins/reload.
            let dynamic_router = service.proxy_router();

            // Register the handler app state
            let app_state = Arc::new(ExternalPluginsAppState {
                service: service.clone(),
                audit_service: context.require_service::<dyn temps_core::AuditLogger>(),
                sensitive_action_authorizer: context
                    .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
            });

            // External plugin OpenAPI schemas would normally be merged into
            // the unified spec at startup. Since discovery now runs in the
            // background, no plugin schemas are available yet — the spec
            // will only include external plugins after a `/x/plugins/reload`
            // call. This trades a more complete first-render spec for a
            // dramatically faster boot, which is the right call when the
            // common case is "no external plugins installed".
            {
                let mut cache = self.cached_schemas.lock().map_err(|error| {
                    PluginError::PluginRegistrationFailed {
                        plugin_name: self.name().to_string(),
                        error: format!("failed to lock external-plugin OpenAPI cache: {error}"),
                    }
                })?;
                *cache = Some(Vec::new());
            }

            context.register_service(service);
            context.register_service(app_state);
            context.register_service(Arc::new(DynamicPluginRouter {
                inner: dynamic_router,
            }));

            tracing::debug!(
                "External plugins service registered (discovery running in background)"
            );
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<ExternalPluginsAppState>();
        let dynamic = context.require_service::<DynamicPluginRouter>();

        // Build the listing + admin routes (/x/plugins, /x/plugins/reload)
        let listing_router = handler::configure_routes().with_state((*app_state).clone());

        let combined_router = listing_router.merge(dynamic.router());

        Some(PluginRoutes::new(combined_router))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Start with the base admin API doc
        let mut combined = ExternalPluginsApiDoc::openapi();

        // Get the cached schemas (populated during register_services)
        let schemas = match self.cached_schemas.lock() {
            Ok(cache) => cache.clone(),
            Err(e) => {
                warn!("Failed to lock cached schemas: {}", e);
                return Some(combined);
            }
        };

        let schemas = match schemas {
            Some(s) => s,
            None => {
                debug!("No cached schemas available yet");
                return Some(combined);
            }
        };

        // Merge each external plugin's schema with path prefixing
        for (plugin_name, plugin_schema) in schemas {
            let prefix = format!("/x/{}", plugin_name);
            debug!(plugin = %plugin_name, prefix = %prefix, "Merging external plugin OpenAPI schema");

            // Prefix all paths in the plugin schema
            let mut prefixed_schema = plugin_schema;
            let mut new_paths = std::collections::BTreeMap::new();
            let old_paths = std::mem::take(&mut prefixed_schema.paths.paths);
            for (path, path_item) in old_paths {
                let prefixed_path = format!("{}{}", prefix, path);
                new_paths.insert(prefixed_path, path_item);
            }
            prefixed_schema.paths.paths = new_paths;

            // Merge using the core merge function
            combined = temps_core::openapi::merge_openapi_schemas(combined, vec![prefixed_schema]);
        }

        Some(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The plugin router is merged into the `/api` router as a catch-all
    /// fallback, so an unknown API path with no plugin behind it must be
    /// the route-not-found problem, not an empty 404.
    #[tokio::test]
    async fn unknown_api_paths_are_a_route_not_found_problem() {
        let dynamic = DynamicPluginRouter {
            inner: Arc::new(RwLock::new(ExternalPluginsService::empty_proxy_router())),
        };
        let app = Router::new().nest("/api", dynamic.router());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/definitely-not-a-route")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router answers");

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(
            problem["type"],
            temps_core::api_problems::ROUTE_NOT_FOUND_PROBLEM_TYPE
        );
        assert!(problem["detail"]
            .as_str()
            .expect("detail")
            .contains("/api/definitely-not-a-route"));
    }

    #[test]
    fn test_plugin_name() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/test"),
            "postgres://localhost/test".to_string(),
        );
        let plugin = ExternalPluginsPlugin::new(config);
        assert_eq!(plugin.name(), "external-plugins");
    }

    #[test]
    fn test_plugin_openapi_schema() {
        let config = ExternalPluginConfig::new(
            PathBuf::from("/tmp/test"),
            "postgres://localhost/test".to_string(),
        );
        let plugin = ExternalPluginsPlugin::new(config);
        let schema = plugin.openapi_schema();
        assert!(schema.is_some());
        let spec = schema.unwrap();
        assert!(spec.paths.paths.contains_key("/x/plugins"));
    }
}
