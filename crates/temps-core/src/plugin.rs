//! Plugin system for modular service registration and route configuration
//!
//! This module provides a trait-based plugin system that enables:
//! - Type-safe service dependency injection
//! - Automatic route registration and OpenAPI aggregation
//! - Clear dependency management with fail-fast error handling
//! - Modular architecture without compile-time coupling

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::extract::Request;
use axum::response::Response;
use axum::{middleware::Next, Router};
use thiserror::Error;
use tracing::debug;
use utoipa::openapi::security::SecurityScheme;
use utoipa::openapi::{ComponentsBuilder, OpenApi};

// Re-export for plugin implementations
pub use axum;
pub use utoipa;

/// Middleware execution priority
/// Lower numbers execute first, higher numbers execute later
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MiddlewarePriority {
    /// Security middleware (authentication, authorization) - executes first
    Security,
    /// Logging and metrics middleware
    Observability,
    /// Request/response transformation middleware
    Transform,
    /// Caching and performance middleware
    Performance,
    /// Business logic middleware
    Business,
    /// Custom middleware with explicit priority
    Custom(u16),
}

impl MiddlewarePriority {
    pub fn value(&self) -> u16 {
        match self {
            MiddlewarePriority::Security => 0,
            MiddlewarePriority::Observability => 100,
            MiddlewarePriority::Transform => 200,
            MiddlewarePriority::Performance => 300,
            MiddlewarePriority::Business => 400,
            MiddlewarePriority::Custom(value) => *value,
        }
    }
}

/// Middleware condition for conditional execution
#[derive(Clone)]
pub enum MiddlewareCondition {
    /// Always execute
    Always,
    /// Execute only for paths matching the pattern
    PathMatches(String),
    /// Execute only for specific HTTP methods
    Methods(Vec<axum::http::Method>),
    /// Execute only when header is present
    HeaderPresent(String),
    /// Execute only when header has specific value
    HeaderEquals(String, String),
    /// Custom condition function
    Custom(Arc<dyn Fn(&Request) -> bool + Send + Sync>),
}

impl std::fmt::Debug for MiddlewareCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Always => write!(f, "Always"),
            Self::PathMatches(pattern) => f.debug_tuple("PathMatches").field(pattern).finish(),
            Self::Methods(methods) => f.debug_tuple("Methods").field(methods).finish(),
            Self::HeaderPresent(header) => f.debug_tuple("HeaderPresent").field(header).finish(),
            Self::HeaderEquals(header, value) => f
                .debug_tuple("HeaderEquals")
                .field(header)
                .field(value)
                .finish(),
            Self::Custom(_) => write!(f, "Custom(<function>)"),
        }
    }
}

impl MiddlewareCondition {
    pub fn matches(&self, req: &Request) -> bool {
        match self {
            MiddlewareCondition::Always => true,
            MiddlewareCondition::PathMatches(pattern) => req.uri().path().contains(pattern),
            MiddlewareCondition::Methods(methods) => methods.contains(req.method()),
            MiddlewareCondition::HeaderPresent(header) => req.headers().contains_key(header),
            MiddlewareCondition::HeaderEquals(header, value) => req
                .headers()
                .get(header)
                .and_then(|v| v.to_str().ok())
                .map(|v| v == value)
                .unwrap_or(false),
            MiddlewareCondition::Custom(func) => func(req),
        }
    }
}

/// Type alias for middleware handler function
pub type MiddlewareHandler = Arc<
    dyn Fn(
            Request,
            Next,
        )
            -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>>
        + Send
        + Sync,
>;

/// Plugin middleware definition
pub struct PluginMiddleware {
    /// Unique name for this middleware
    pub name: String,
    /// Plugin that provides this middleware
    pub plugin_name: String,
    /// Execution priority
    pub priority: MiddlewarePriority,
    /// Condition for when to execute
    pub condition: MiddlewareCondition,
    /// The actual middleware function
    pub handler: MiddlewareHandler,
}

impl std::fmt::Debug for PluginMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginMiddleware")
            .field("name", &self.name)
            .field("plugin_name", &self.plugin_name)
            .field("priority", &self.priority)
            .field("condition", &self.condition)
            .field("handler", &"<function>")
            .finish()
    }
}

/// Trait for middleware that can access plugin services and context
pub trait TempsMiddleware: Send + Sync {
    /// The name of this middleware
    fn name(&self) -> &'static str;

    /// The plugin name that provides this middleware
    fn plugin_name(&self) -> &'static str;

    /// Priority for execution order
    fn priority(&self) -> MiddlewarePriority {
        MiddlewarePriority::Business
    }

    /// Condition for when to execute
    fn condition(&self) -> MiddlewareCondition {
        MiddlewareCondition::Always
    }

    /// Initialize the middleware with access to the plugin context
    /// This is called once during plugin initialization
    fn initialize(&mut self, context: &PluginContext) -> Result<(), PluginError> {
        let _ = context; // Default implementation ignores context
        Ok(())
    }

    /// Execute the middleware with access to request and next handler
    fn execute<'a>(
        &'a self,
        req: Request,
        next: Next,
    ) -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'a>>;
}

/// Helper struct to wrap TempsMiddleware implementations
pub struct TempsMiddlewareWrapper {
    middleware: Arc<dyn TempsMiddleware>,
}

impl TempsMiddlewareWrapper {
    pub fn new(middleware: Arc<dyn TempsMiddleware>) -> Self {
        Self { middleware }
    }

    /// Convert to PluginMiddleware for use in the existing system
    pub fn into_plugin_middleware(self) -> PluginMiddleware {
        let name = self.middleware.name().to_string();
        let plugin_name = self.middleware.plugin_name().to_string();
        let priority = self.middleware.priority();
        let condition = self.middleware.condition();

        let middleware = self.middleware.clone();
        let handler = Arc::new(
            move |req: Request,
                  next: Next|
                  -> Pin<
                Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>,
            > {
                let middleware = middleware.clone();
                Box::pin(async move { middleware.execute(req, next).await })
            },
        );

        PluginMiddleware {
            name,
            plugin_name,
            priority,
            condition,
            handler,
        }
    }
}

/// Collection of middleware from a plugin
pub struct PluginMiddlewareCollection {
    pub middleware: Vec<PluginMiddleware>,
}

impl Default for PluginMiddlewareCollection {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginMiddlewareCollection {
    pub fn new() -> Self {
        Self {
            middleware: Vec::new(),
        }
    }

    pub fn add_middleware(
        &mut self,
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        priority: MiddlewarePriority,
        condition: MiddlewareCondition,
        handler: impl Fn(
                Request,
                Next,
            )
                -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>>
            + Send
            + Sync
            + 'static,
    ) {
        self.middleware.push(PluginMiddleware {
            name: name.into(),
            plugin_name: plugin_name.into(),
            priority,
            condition,
            handler: Arc::new(handler),
        });
    }

    /// Add a TempsMiddleware implementation
    pub fn add_temps_middleware(&mut self, middleware: Arc<dyn TempsMiddleware>) {
        let wrapper = TempsMiddlewareWrapper::new(middleware);
        self.middleware.push(wrapper.into_plugin_middleware());
    }

    /// Add simple middleware that always executes
    pub fn add_simple_middleware<F, Fut>(
        &mut self,
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        priority: MiddlewarePriority,
        handler: F,
    ) where
        F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'static,
    {
        self.add_middleware(
            name,
            plugin_name,
            priority,
            MiddlewareCondition::Always,
            move |req, next| Box::pin(handler(req, next)),
        );
    }

    /// Add middleware that only executes for specific paths
    pub fn add_path_middleware<F, Fut>(
        &mut self,
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        priority: MiddlewarePriority,
        path_pattern: impl Into<String>,
        handler: F,
    ) where
        F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'static,
    {
        self.add_middleware(
            name,
            plugin_name,
            priority,
            MiddlewareCondition::PathMatches(path_pattern.into()),
            move |req, next| Box::pin(handler(req, next)),
        );
    }

    /// Add authentication middleware
    pub fn add_auth_middleware<F, Fut>(
        &mut self,
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        handler: F,
    ) where
        F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'static,
    {
        self.add_simple_middleware(name, plugin_name, MiddlewarePriority::Security, handler);
    }

    /// Add logging/metrics middleware
    pub fn add_observability_middleware<F, Fut>(
        &mut self,
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        handler: F,
    ) where
        F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Response, axum::http::StatusCode>> + Send + 'static,
    {
        self.add_simple_middleware(
            name,
            plugin_name,
            MiddlewarePriority::Observability,
            handler,
        );
    }
}

/// Errors that can occur during plugin operations
#[derive(Error, Debug)]
pub enum PluginError {
    #[error("Plugin registration failed for '{plugin_name}': {error}")]
    PluginRegistrationFailed { plugin_name: String, error: String },

    #[error("Service '{service_type}' is required but not registered")]
    ServiceNotFound { service_type: String },

    #[error("Plugin state '{plugin_name}' not found")]
    PluginStateNotFound { plugin_name: String },

    #[error("Failed to initialize plugin system: {0}")]
    InitializationFailed(String),

    #[error("OpenAPI schema merge failed: {0}")]
    OpenApiMergeFailed(String),
}

/// Core plugin trait that defines the plugin interface
pub trait TempsPlugin: Send + Sync {
    /// Unique identifier for this plugin
    fn name(&self) -> &'static str;

    /// Register services that this plugin provides
    ///
    /// Use `context.require_service::<T>()` to get dependencies.
    /// Use `context.register_service(service)` to provide services for other plugins.
    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>>;

    /// Configure HTTP routes for this plugin
    ///
    /// Return None if this plugin doesn't provide HTTP endpoints.
    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        None
    }

    /// Provide OpenAPI schema for this plugin's endpoints
    ///
    /// Return None if this plugin doesn't have API documentation.
    fn openapi_schema(&self) -> Option<OpenApi> {
        None
    }

    /// Configure middleware for this plugin
    ///
    /// Return None if this plugin doesn't provide middleware.
    fn configure_middleware(&self, _context: &PluginContext) -> Option<PluginMiddlewareCollection> {
        None
    }
}

/// Route configuration returned by plugins
pub struct PluginRoutes {
    /// The actual router with handlers
    pub router: Router,
}

impl PluginRoutes {
    /// Create plugin routes with no path prefix
    pub fn new(router: Router) -> Self {
        Self { router }
    }
}

/// Type-safe service registry for dependency injection
pub struct ServiceRegistry {
    services: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceRegistry {
    /// Create a new service registry
    pub fn new() -> Self {
        Self {
            services: Mutex::new(HashMap::new()),
        }
    }

    /// Register a service for other plugins to use
    pub fn register<T: Send + Sync + 'static + ?Sized>(&self, service: Arc<T>) {
        debug!("Registering service: {}", std::any::type_name::<T>());
        self.services
            .lock()
            .unwrap()
            .insert(TypeId::of::<T>(), Box::new(service));
    }

    /// Get a service if it's registered
    pub fn get<T: Send + Sync + 'static + ?Sized>(&self) -> Option<Arc<T>> {
        self.services
            .lock()
            .unwrap()
            .get(&TypeId::of::<T>())
            .and_then(|any| any.downcast_ref::<Arc<T>>())
            .cloned()
    }

    /// Require a service - panics with helpful error if not available
    pub fn require<T: Send + Sync + 'static + ?Sized>(&self) -> Arc<T> {
        self.get::<T>().unwrap_or_else(|| {
            panic!(
                "Service '{}' is required but not registered. \
                 Make sure the plugin providing this service is registered before plugins that depend on it.",
                std::any::type_name::<T>()
            )
        })
    }
}

/// Registry for plugin-specific state (used for routing)
pub struct PluginStateRegistry {
    states: Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>,
}

impl Default for PluginStateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginStateRegistry {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Register plugin state for route configuration
    pub fn register_state<T: Send + Sync + 'static + ?Sized>(
        &self,
        plugin_name: &str,
        state: Arc<T>,
    ) {
        debug!("Registering plugin state for: {}", plugin_name);
        self.states
            .lock()
            .unwrap()
            .insert(plugin_name.to_string(), Box::new(state));
    }

    /// Get plugin state for route configuration
    pub fn get_state<T: Send + Sync + 'static + ?Sized>(
        &self,
        plugin_name: &str,
    ) -> Option<Arc<T>> {
        self.states
            .lock()
            .unwrap()
            .get(plugin_name)
            .and_then(|any| any.downcast_ref::<Arc<T>>())
            .cloned()
    }
}

/// Context provided to plugins for service access and registration
pub struct PluginContext {
    service_registry: Arc<ServiceRegistry>,
    state_registry: Arc<PluginStateRegistry>,
}

impl PluginContext {
    pub fn new(registry: Arc<ServiceRegistry>, state_registry: Arc<PluginStateRegistry>) -> Self {
        Self {
            service_registry: registry,
            state_registry,
        }
    }

    /// Get a service if it's available (for optional dependencies)
    pub fn get_service<T: Send + Sync + 'static + ?Sized>(&self) -> Option<Arc<T>> {
        self.service_registry.get::<T>()
    }

    /// Require a service - panics with clear error if not available
    pub fn require_service<T: Send + Sync + 'static + ?Sized>(&self) -> Arc<T> {
        self.service_registry.require::<T>()
    }

    /// This method is not available on read-only context
    /// Use ServiceRegistrationContext during plugin initialization instead
    pub fn register_service<T: Send + Sync + 'static + ?Sized>(&self, _service: Arc<T>) {
        panic!("register_service is not available on read-only PluginContext");
    }

    /// This method is not available on read-only context
    /// Use ServiceRegistrationContext during plugin initialization instead
    pub fn register_plugin_state<T: Send + Sync + 'static>(
        &self,
        _plugin_name: &str,
        _state: Arc<T>,
    ) {
        panic!("register_plugin_state is not available on read-only PluginContext");
    }

    /// Get plugin state for route configuration
    pub fn get_plugin_state<T: Send + Sync + 'static + ?Sized>(
        &self,
        plugin_name: &str,
    ) -> Option<Arc<T>> {
        self.state_registry.get_state::<T>(plugin_name)
    }
}

/// Special context for service registration that allows mutable access
pub struct ServiceRegistrationContext {
    service_registry: Arc<ServiceRegistry>,
    state_registry: Arc<PluginStateRegistry>,
}

impl Default for ServiceRegistrationContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceRegistrationContext {
    pub fn new() -> Self {
        Self {
            service_registry: Arc::new(ServiceRegistry::new()),
            state_registry: Arc::new(PluginStateRegistry::new()),
        }
    }

    /// Register a service for other plugins to use
    pub fn register_service<T: Send + Sync + 'static + ?Sized>(&self, service: Arc<T>) {
        self.service_registry.register(service);
    }

    /// Register plugin state for route configuration
    pub fn register_plugin_state<T: Send + Sync + 'static + ?Sized>(
        &self,
        plugin_name: &str,
        state: Arc<T>,
    ) {
        self.state_registry.register_state(plugin_name, state);
    }

    /// Get a service if it's available (for dependencies)
    pub fn get_service<T: Send + Sync + 'static + ?Sized>(&self) -> Option<Arc<T>> {
        self.service_registry.get::<T>()
    }

    /// Require a service - panics with clear error if not available
    pub fn require_service<T: Send + Sync + 'static + ?Sized>(&self) -> Arc<T> {
        self.service_registry.require::<T>()
    }

    /// Create a read-only context for plugin operations
    pub fn create_plugin_context(&self) -> PluginContext {
        PluginContext::new(self.service_registry.clone(), self.state_registry.clone())
    }
}

/// Main plugin manager that handles plugin registration, initialization, and application building
pub struct PluginManager {
    plugins: Vec<Box<dyn TempsPlugin>>,
    context: ServiceRegistrationContext,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginManager {
    /// Create a new plugin manager
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            context: ServiceRegistrationContext::new(),
        }
    }

    /// Register a plugin (order matters for dependencies)
    pub fn register_plugin(&mut self, plugin: Box<dyn TempsPlugin>) {
        debug!("Registering plugin: {}", plugin.name());
        self.plugins.push(plugin);
    }

    /// Initialize all plugins in registration order
    pub async fn initialize_plugins(&mut self) -> Result<(), PluginError> {
        debug!("Initializing {} plugins", self.plugins.len());

        for plugin in &self.plugins {
            debug!("Initializing plugin: {}", plugin.name());

            plugin.register_services(&self.context).await.map_err(|e| {
                PluginError::PluginRegistrationFailed {
                    plugin_name: plugin.name().to_string(),
                    error: e.to_string(),
                }
            })?;

            debug!("Successfully initialized plugin: {}", plugin.name());
        }

        Ok(())
    }

    /// Build the complete application with routes, middleware, and OpenAPI
    pub fn build_application(&self) -> Result<Router, PluginError> {
        debug!("Building application with {} plugins", self.plugins.len());

        let plugin_context = self.context.create_plugin_context();
        let mut api_router = Router::new();

        // Collect routes from all plugins
        for plugin in &self.plugins {
            if let Some(plugin_routes) = plugin.configure_routes(&plugin_context) {
                debug!("Adding routes for plugin: {}", plugin.name());
                api_router = api_router.merge(plugin_routes.router);
            }
        }

        // Collect and apply middleware from all plugins
        let middleware = self.collect_middleware(&plugin_context);
        api_router = self.apply_middleware_to_router(api_router, middleware);

        // Build unified OpenAPI documentation
        let _openapi_schema = self.build_unified_openapi()?;
        let docs_router = Router::new();

        // Combine everything
        let app = Router::new().nest("/api", api_router).merge(docs_router);

        Ok(app)
    }

    /// Get the unified OpenAPI schema from all plugins
    pub fn get_unified_openapi(&self) -> Result<OpenApi, PluginError> {
        self.build_unified_openapi()
    }

    /// Get all middleware from plugins for inspection
    pub fn get_middleware(&self) -> Vec<PluginMiddleware> {
        let plugin_context = self.context.create_plugin_context();
        self.collect_middleware(&plugin_context)
    }

    /// Build unified OpenAPI schema from all plugins
    fn build_unified_openapi(&self) -> Result<OpenApi, PluginError> {
        use utoipa::openapi::*;

        let mut combined_openapi = OpenApiBuilder::new()
            .info(
                InfoBuilder::new()
                    .title("Temps")
                    .description(Some("A comprehensive API for managing projects, deployments, and infrastructure resources"))
                    .version("1.0.0")
                    .contact(Some(
                        ContactBuilder::new()
                            .name(Some("Temps Support"))
                            .url(Some("https://temps.sh"))
                            .build()
                    ))
                    .build()
            )
            .servers(Some(vec![
                ServerBuilder::new()
                    .url("/api")
                    .description(Some("Base path for all API endpoints"))
                    .build()
            ]))
            .components(Some(
                ComponentsBuilder::new()
                    .security_scheme("bearer_auth", self.create_bearer_auth_scheme())
                    .build()
            ))
            .build();

        // Merge OpenAPI schemas from all plugins
        for plugin in &self.plugins {
            if let Some(plugin_openapi) = plugin.openapi_schema() {
                debug!("Merging OpenAPI schema for plugin: {}", plugin.name());
                combined_openapi = self.merge_openapi_schemas(combined_openapi, plugin_openapi)?;
            }
        }

        Ok(combined_openapi)
    }

    /// Merge two OpenAPI schemas
    fn merge_openapi_schemas(
        &self,
        mut base: OpenApi,
        plugin_schema: OpenApi,
    ) -> Result<OpenApi, PluginError> {
        // Merge paths - plugin_schema.paths is not Option<Paths>, it's just Paths
        for (path, path_item) in plugin_schema.paths.paths {
            base.paths.paths.insert(path, path_item);
        }

        // Merge components
        if let Some(plugin_components) = plugin_schema.components {
            let base_components = base
                .components
                .get_or_insert_with(|| ComponentsBuilder::new().build());

            // Merge schemas - plugin_components.schemas is not Option
            for (name, schema) in plugin_components.schemas {
                base_components.schemas.insert(name, schema);
            }

            // Merge responses - plugin_components.responses is not Option
            for (name, response) in plugin_components.responses {
                base_components.responses.insert(name, response);
            }
        }

        // Merge tags
        if let Some(plugin_tags) = plugin_schema.tags {
            let base_tags = base.tags.get_or_insert_with(Vec::new);
            base_tags.extend(plugin_tags);
        }

        Ok(base)
    }

    /// Create bearer authentication scheme for OpenAPI
    fn create_bearer_auth_scheme(&self) -> SecurityScheme {
        use utoipa::openapi::security::*;

        let mut http_scheme = Http::new(HttpAuthScheme::Bearer);
        http_scheme.description = Some(
            "Bearer token authentication. Use format: `Bearer <your-token>`. Supports API keys (starting with `tk_`), CLI tokens, and session tokens.".to_string()
        );

        SecurityScheme::Http(http_scheme)
    }

    /// Get access to the service registration context for manual service registration
    /// This is typically used before plugin initialization to register core services
    pub fn service_context(&self) -> &ServiceRegistrationContext {
        &self.context
    }

    /// Get access to the service registry for testing
    #[cfg(test)]
    pub fn service_registry(&self) -> &ServiceRegistrationContext {
        &self.context
    }

    /// Collect middleware from all plugins
    fn collect_middleware(&self, plugin_context: &PluginContext) -> Vec<PluginMiddleware> {
        let mut all_middleware = Vec::new();

        for plugin in &self.plugins {
            if let Some(middleware_collection) = plugin.configure_middleware(plugin_context) {
                debug!("Collecting middleware from plugin: {}", plugin.name());
                all_middleware.extend(middleware_collection.middleware);
            }
        }

        // Sort middleware by priority (lower numbers execute first)
        all_middleware.sort_by_key(|mw| mw.priority.value());

        debug!("Collected {} middleware from plugins", all_middleware.len());
        for mw in &all_middleware {
            debug!(
                "  - {} (priority: {}) from {}",
                mw.name,
                mw.priority.value(),
                mw.plugin_name
            );
        }

        all_middleware
    }

    /// Apply collected middleware to a router
    fn apply_middleware_to_router(
        &self,
        mut router: Router,
        middleware: Vec<PluginMiddleware>,
    ) -> Router {
        for mw in middleware {
            debug!(
                "Applying middleware: {} from plugin: {}",
                mw.name, mw.plugin_name
            );

            let handler = mw.handler.clone();
            let condition = mw.condition.clone();

            router = router.layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let handler = handler.clone();
                    let condition = condition.clone();

                    async move {
                        if condition.matches(&req) {
                            handler(req, next).await
                        } else {
                            Ok(next.run(req).await)
                        }
                    }
                },
            ));
        }

        router
    }
}

/// Macro to simplify middleware creation
#[macro_export]
macro_rules! middleware {
    (
        name: $name:expr,
        plugin: $plugin:expr,
        priority: $priority:expr,
        condition: $condition:expr,
        handler: $handler:expr
    ) => {
        PluginMiddleware {
            name: $name.into(),
            plugin_name: $plugin.into(),
            priority: $priority,
            condition: $condition,
            handler: std::sync::Arc::new($handler),
        }
    };

    (
        name: $name:expr,
        plugin: $plugin:expr,
        priority: $priority:expr,
        handler: $handler:expr
    ) => {
        middleware!(
            name: $name,
            plugin: $plugin,
            priority: $priority,
            condition: MiddlewareCondition::Always,
            handler: $handler
        )
    };

    (
        name: $name:expr,
        plugin: $plugin:expr,
        handler: $handler:expr
    ) => {
        middleware!(
            name: $name,
            plugin: $plugin,
            priority: MiddlewarePriority::Business,
            handler: $handler
        )
    };
}

/// Helper functions for common middleware patterns
pub mod middleware_helpers {
    use super::*;

    /// Create a logging middleware
    pub fn logging_middleware(
        plugin_name: &str,
    ) -> impl Fn(
        Request,
        Next,
    ) -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>>
           + Send
           + Sync {
        let plugin_name = plugin_name.to_string();
        move |req: Request, next: Next| {
            let plugin_name = plugin_name.clone();
            Box::pin(async move {
                let method = req.method().clone();
                let uri = req.uri().clone();
                let start = std::time::Instant::now();

                debug!("[{}] {} {} - Request started", plugin_name, method, uri);

                let response = next.run(req).await;
                let duration = start.elapsed();

                debug!(
                    "[{}] {} {} - Response: {} ({:?})",
                    plugin_name,
                    method,
                    uri,
                    response.status(),
                    duration
                );

                Ok(response)
            })
        }
    }

    /// Create a request ID middleware
    pub fn request_id_middleware(
        _plugin_name: &str,
    ) -> impl Fn(
        Request,
        Next,
    ) -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>>
           + Send
           + Sync {
        move |mut req: Request, next: Next| {
            Box::pin(async move {
                // Add request ID if not present
                let request_id = if !req.headers().contains_key("x-request-id") {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    req.headers_mut().insert(
                        "x-request-id",
                        axum::http::HeaderValue::from_str(&request_id).unwrap(),
                    );
                    Some(request_id)
                } else {
                    req.headers()
                        .get("x-request-id")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string())
                };

                let mut response = next.run(req).await;

                // Add request ID to response if not already present
                if let Some(req_id) = request_id {
                    if !response.headers().contains_key("x-request-id") {
                        if let Ok(header_value) = axum::http::HeaderValue::from_str(&req_id) {
                            response.headers_mut().insert("x-request-id", header_value);
                        }
                    }
                }

                Ok(response)
            })
        }
    }

    /// Create CORS middleware
    pub fn cors_middleware(
        _plugin_name: &str,
        allowed_origins: Vec<String>,
    ) -> impl Fn(
        Request,
        Next,
    ) -> Pin<Box<dyn Future<Output = Result<Response, axum::http::StatusCode>> + Send>>
           + Send
           + Sync {
        move |req: Request, next: Next| {
            let allowed_origins = allowed_origins.clone();
            Box::pin(async move {
                let origin = req
                    .headers()
                    .get("origin")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());

                let mut response = next.run(req).await;

                // Add CORS headers
                if let Some(origin) = origin {
                    if allowed_origins.contains(&origin)
                        || allowed_origins.contains(&"*".to_string())
                    {
                        response.headers_mut().insert(
                            "access-control-allow-origin",
                            axum::http::HeaderValue::from_str(&origin).unwrap(),
                        );
                    }
                }

                response.headers_mut().insert(
                    "access-control-allow-methods",
                    axum::http::HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
                );
                response.headers_mut().insert(
                    "access-control-allow-headers",
                    axum::http::HeaderValue::from_static("Content-Type, Authorization"),
                );

                Ok(response)
            })
        }
    }
}
