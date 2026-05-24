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
use std::sync::{Arc, RwLock};

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
    /// Whether this middleware should also be applied to the public ingest
    /// router (e.g. session-replay init, analytics events). Defaults to
    /// `false` — only the admin router gets middleware unless explicitly
    /// opted in. Request-metadata injection must opt in; auth must not.
    pub apply_to_public: bool,
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
            .field("apply_to_public", &self.apply_to_public)
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

    /// Whether this middleware should also be applied to the public ingest
    /// router. Default is `false` — middleware only runs on the admin router
    /// unless it explicitly opts in. Request-metadata injection opts in;
    /// auth must stay opted out.
    fn apply_to_public(&self) -> bool {
        false
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
        let apply_to_public = self.middleware.apply_to_public();

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
            apply_to_public,
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
            apply_to_public: false,
            handler: Arc::new(handler),
        });
    }

    /// Same as [`Self::add_middleware`] but also applies the middleware to
    /// the public ingest router. Use for request-context injection that
    /// public handlers (no auth) still depend on, e.g. `RequestMetadata`.
    pub fn add_shared_middleware(
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
            apply_to_public: true,
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

    /// Initialize plugin-managed services after all plugins are registered
    ///
    /// This is called after all plugins have registered their services, allowing
    /// plugins to access services from other plugins (like ExternalServiceManager)
    /// to initialize their own services with configuration from the database.
    ///
    /// Use this hook when you need to load service configuration from the database
    /// or perform other async initialization that requires access to other plugins' services.
    fn initialize_plugin_services<'a>(
        &'a self,
        _context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }

    /// Configure HTTP routes for this plugin
    ///
    /// Return None if this plugin doesn't provide HTTP endpoints.
    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        None
    }

    /// Configure public HTTP routes that don't require authentication.
    ///
    /// These routes are served under /api but bypass auth middleware.
    /// Use for tracking pixels, webhooks, and other public endpoints.
    fn configure_public_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
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

/// A single `(METHOD, PATH)` claim that replaces whatever an earlier-loaded
/// plugin (typically OSS) bound for the same pair.
///
/// Axum 0.8 panics if two routers register the same `(method, path)` via
/// `Router::merge`. That makes naive route replacement impossible. The
/// override mechanism sidesteps the merge entirely: matching requests are
/// dispatched by a wrapper middleware layer applied above the merged router,
/// so the additive route is registered but never reached.
///
/// `path` is the path inside the listener's router — without the `/api`
/// prefix that `build_application` applies at the end. Example:
/// `"/auth/login"`, not `"/api/auth/login"`.
pub struct RouteOverride {
    pub method: axum::http::Method,
    pub path: String,
    pub handler: OverrideHandler,
}

impl std::fmt::Debug for RouteOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteOverride")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("handler", &"<async fn>")
            .finish()
    }
}

/// Async function that produces the override response. Takes the original
/// request, returns a response. No `Next` parameter — overrides are terminal
/// by definition; they own the request fully.
pub type OverrideHandler =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// Route configuration returned by plugins.
///
/// `router` is the *additive* router — routes that get merged into the
/// listener's router as new endpoints. Path collisions on `router` panic
/// (Axum's normal behavior).
///
/// `overrides` is the *replacement* list — `(method, path)` pairs this
/// plugin claims exclusive control over. When two plugins claim the same
/// pair, the last-registered plugin wins and a warning is logged; an
/// explicit override always wins against any additive route registered for
/// the same pair.
///
/// The field `overrides` is intentionally `pub(crate)` — plugins construct
/// `PluginRoutes` through `new()` + `with_override()`, never via a struct
/// literal. This keeps the override aggregation contract enforceable inside
/// `temps-core` and prevents accidental bypass.
pub struct PluginRoutes {
    /// Additive router merged into the host listener's router.
    pub router: Router,
    /// `(method, path)` pairs this plugin replaces.
    pub(crate) overrides: Vec<RouteOverride>,
}

impl PluginRoutes {
    /// Create plugin routes with no overrides. Existing additive-only
    /// plugins (the OSS default) use this — no behavior change.
    pub fn new(router: Router) -> Self {
        Self {
            router,
            overrides: Vec::new(),
        }
    }

    /// Declare that this plugin owns `(method, path)`. The override handler
    /// is called instead of any additive route registered for the same pair
    /// (whether by this plugin, an OSS plugin, or another EE plugin).
    ///
    /// Last call wins on collision; a `tracing::warn!` is emitted when the
    /// override aggregator sees the same pair declared twice.
    pub fn with_override<F, Fut>(
        mut self,
        method: axum::http::Method,
        path: impl Into<String>,
        handler: F,
    ) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: OverrideHandler = Arc::new(move |req: Request| {
            let fut = handler(req);
            Box::pin(fut) as Pin<Box<dyn Future<Output = Response> + Send>>
        });
        self.overrides.push(RouteOverride {
            method,
            path: path.into(),
            handler: boxed,
        });
        self
    }
}

/// Drop overridden paths from a plugin's OpenAPI contribution unless the
/// plugin itself is the one that claimed them. After this, only the
/// override-declaring plugin can publish a `PathItem` for paths in
/// `overridden_paths` — guaranteeing the merged OpenAPI matches what the
/// runtime dispatcher actually serves.
fn filter_overridden_paths(
    mut schema: utoipa::openapi::OpenApi,
    overridden_paths: &std::collections::HashSet<String>,
    plugin_name: &str,
    path_owner_by_plugin: &std::collections::HashSet<(String, String)>,
) -> utoipa::openapi::OpenApi {
    if overridden_paths.is_empty() {
        return schema;
    }
    let owned_paths: std::collections::HashSet<&String> = path_owner_by_plugin
        .iter()
        .filter_map(|(p, path)| (p == plugin_name).then_some(path))
        .collect();
    schema.paths.paths.retain(|path, _item| {
        // Keep paths that aren't overridden at all, or that this plugin owns.
        !overridden_paths.contains(path) || owned_paths.contains(path)
    });
    schema
}

/// Apply a set of `(method, path)` overrides as a middleware layer wrapping
/// `router`. The layer matches each incoming request against the override
/// map; on hit it dispatches directly to the override handler, on miss it
/// calls `next.run(req).await` to let the additive router serve as usual.
///
/// Why a middleware layer and not Axum routes: registering both an OSS route
/// and an EE override for the same `(method, path)` via `Router::merge`
/// panics on `Router::merge` -> `MethodRouter::merge_for_path` "Overlapping
/// method route" (axum 0.8 `routing/method_routing.rs:1052`). The layer
/// intercepts before axum's matcher runs, so the additive route is still
/// registered (no merge conflict) but never reached for overridden pairs.
///
/// `overrides` map is `(Method, Path) -> (declaring_plugin_name, handler)`.
/// The plugin name is kept for `Debug`/discovery; the dispatcher only needs
/// the handler.
fn apply_route_overrides(
    router: Router,
    overrides: HashMap<(axum::http::Method, String), (String, OverrideHandler)>,
) -> Router {
    if overrides.is_empty() {
        return router;
    }
    let map: Arc<HashMap<(axum::http::Method, String), OverrideHandler>> = Arc::new(
        overrides
            .into_iter()
            .map(|(k, (_plugin, handler))| (k, handler))
            .collect(),
    );
    router.layer(axum::middleware::from_fn(
        move |req: Request, next: Next| {
            let map = map.clone();
            async move {
                let key = (req.method().clone(), req.uri().path().to_string());
                if let Some(handler) = map.get(&key) {
                    let handler = handler.clone();
                    Ok::<Response, axum::http::StatusCode>(handler(req).await)
                } else {
                    Ok(next.run(req).await)
                }
            }
        },
    ))
}

/// Two-listener router split produced by [`PluginManager::build_split_application`].
///
/// - `public` is mounted on the public-facing console listener and contains
///   only endpoints that are safe to expose to the internet without an
///   admin-network gate (event ingestion, AI gateway, sentry DSN ingest, etc.).
/// - `admin` is mounted on the admin listener and contains every other
///   route (dashboard queries, CRUD management, settings).
pub struct SplitApplication {
    pub public: Router,
    pub admin: Router,
}

/// Type-safe service registry for dependency injection
pub struct ServiceRegistry {
    services: RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
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
            services: RwLock::new(HashMap::new()),
        }
    }

    /// Register a service for other plugins to use
    pub fn register<T: Send + Sync + 'static + ?Sized>(&self, service: Arc<T>) {
        debug!("Registering service: {}", std::any::type_name::<T>());
        self.services
            .write()
            .unwrap()
            .insert(TypeId::of::<T>(), Box::new(service));
    }

    /// Get a service if it's registered
    pub fn get<T: Send + Sync + 'static + ?Sized>(&self) -> Option<Arc<T>> {
        self.services
            .read()
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
    states: RwLock<HashMap<String, Box<dyn Any + Send + Sync>>>,
}

impl Default for PluginStateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginStateRegistry {
    pub fn new() -> Self {
        Self {
            states: RwLock::new(HashMap::new()),
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
            .write()
            .unwrap()
            .insert(plugin_name.to_string(), Box::new(state));
    }

    /// Get plugin state for route configuration
    pub fn get_state<T: Send + Sync + 'static + ?Sized>(
        &self,
        plugin_name: &str,
    ) -> Option<Arc<T>> {
        self.states
            .read()
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

        // Phase 1: Register all services
        for plugin in &self.plugins {
            debug!("Registering services for plugin: {}", plugin.name());

            plugin.register_services(&self.context).await.map_err(|e| {
                PluginError::PluginRegistrationFailed {
                    plugin_name: plugin.name().to_string(),
                    error: e.to_string(),
                }
            })?;

            debug!(
                "Successfully registered services for plugin: {}",
                plugin.name()
            );
        }

        // Phase 2: Initialize plugin services (after all services are registered)
        // This allows plugins to access services from other plugins
        let plugin_context = self.context.create_plugin_context();
        for plugin in &self.plugins {
            debug!("Initializing plugin services for: {}", plugin.name());

            plugin
                .initialize_plugin_services(&plugin_context)
                .await
                .map_err(|e| PluginError::PluginRegistrationFailed {
                    plugin_name: plugin.name().to_string(),
                    error: format!("Failed to initialize plugin services: {}", e),
                })?;

            debug!(
                "Successfully initialized plugin services for: {}",
                plugin.name()
            );
        }

        Ok(())
    }

    /// Build the complete application with routes, middleware, and OpenAPI as
    /// a single combined router. Used in single-listener (backwards-compat)
    /// mode where every route binds to the same address.
    pub fn build_application(&self) -> Result<Router, PluginError> {
        let split = self.build_split_application()?;
        let app = Router::new()
            .nest("/api", split.public)
            .nest("/api", split.admin);
        Ok(app)
    }

    /// Build the application as separate public and admin routers, ready to
    /// be mounted on different listeners. Neither router has the `/api`
    /// prefix applied yet — the caller is responsible for `.nest("/api", ...)`
    /// (or any other base path) when wiring them into `axum::serve`.
    ///
    /// - The admin router has plugin middleware applied (auth, audit, etc.).
    /// - The public router has no middleware — public ingest endpoints
    ///   authenticate themselves via API key / DSN tokens / Host header
    ///   lookups inside their handlers.
    pub fn build_split_application(&self) -> Result<SplitApplication, PluginError> {
        debug!(
            "Building split application with {} plugins",
            self.plugins.len()
        );

        let plugin_context = self.context.create_plugin_context();
        let mut admin_router = Router::new();
        let mut public_router = Router::new();
        // Aggregated overrides — last-registered wins. Separate maps for the
        // admin and public listeners; an override declared in
        // `configure_routes` overrides admin paths only, and vice versa.
        let mut admin_overrides: HashMap<(axum::http::Method, String), (String, OverrideHandler)> =
            HashMap::new();
        let mut public_overrides: HashMap<(axum::http::Method, String), (String, OverrideHandler)> =
            HashMap::new();

        for plugin in &self.plugins {
            if let Some(plugin_routes) = plugin.configure_routes(&plugin_context) {
                debug!("Adding admin routes for plugin: {}", plugin.name());
                admin_router = admin_router.merge(plugin_routes.router);
                for ov in plugin_routes.overrides {
                    let key = (ov.method.clone(), ov.path.clone());
                    if let Some((prev, _)) = admin_overrides.get(&key) {
                        tracing::warn!(
                            "Plugin '{}' overrides admin route {} {} previously claimed by '{}' — last-loaded wins",
                            plugin.name(),
                            ov.method,
                            ov.path,
                            prev,
                        );
                    }
                    admin_overrides.insert(key, (plugin.name().to_string(), ov.handler));
                }
            }
            if let Some(public_routes) = plugin.configure_public_routes(&plugin_context) {
                debug!("Adding public routes for plugin: {}", plugin.name());
                public_router = public_router.merge(public_routes.router);
                for ov in public_routes.overrides {
                    let key = (ov.method.clone(), ov.path.clone());
                    if let Some((prev, _)) = public_overrides.get(&key) {
                        tracing::warn!(
                            "Plugin '{}' overrides public route {} {} previously claimed by '{}' — last-loaded wins",
                            plugin.name(),
                            ov.method,
                            ov.path,
                            prev,
                        );
                    }
                    public_overrides.insert(key, (plugin.name().to_string(), ov.handler));
                }
            }
        }

        // Mount the override interceptor *inside* both routers, before
        // middleware is layered on top. This way auth / request-metadata
        // middleware still wraps overrides — they're real routes from the
        // request lifecycle's perspective, just dispatched through a single
        // hashmap-backed layer instead of axum's route table.
        admin_router = apply_route_overrides(admin_router, admin_overrides);
        public_router = apply_route_overrides(public_router, public_overrides);

        let middleware = self.collect_middleware(&plugin_context);

        // Middleware that opts into `apply_to_public` (e.g. request metadata
        // injection) must run on both routers — public ingest endpoints
        // depend on the same `Extension<RequestMetadata>` as admin handlers,
        // even though they skip auth. Other middleware (auth) stays
        // admin-only.
        let public_middleware: Vec<PluginMiddleware> = middleware
            .iter()
            .filter(|mw| mw.apply_to_public)
            .map(|mw| PluginMiddleware {
                name: mw.name.clone(),
                plugin_name: mw.plugin_name.clone(),
                priority: mw.priority,
                condition: mw.condition.clone(),
                apply_to_public: mw.apply_to_public,
                handler: mw.handler.clone(),
            })
            .collect();
        public_router = self.apply_middleware_to_router(public_router, public_middleware);
        admin_router = self.apply_middleware_to_router(admin_router, middleware);

        Ok(SplitApplication {
            public: public_router,
            admin: admin_router,
        })
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

    /// Build unified OpenAPI schema from all plugins.
    ///
    /// Override-aware: when a plugin declares a `RouteOverride` on
    /// `(method, path)`, that plugin's OpenAPI `PathItem` for `path` wins
    /// regardless of plugin load order. Without this, an OSS plugin
    /// registered after the EE override plugin would still publish its own
    /// schema for the overridden path — and the generated SDK would lie
    /// about the request/response shape clients should actually see.
    ///
    /// Note that the override map is keyed by `(method, path)` but OpenAPI
    /// `PathItem`s aggregate all methods for a path. If an EE plugin
    /// overrides only `POST /auth/login` but the OSS plugin also exposes
    /// `GET /auth/login` (e.g. a redirect helper), the EE plugin's
    /// `PathItem` will replace OSS's entirely — losing the `GET`. This
    /// matches the v1 runtime semantics ("overrides own the whole path
    /// across all methods declared in their plugin's openapi schema") and is
    /// documented; an EE override should re-declare any method on the path
    /// it wants to preserve from OSS.
    fn build_unified_openapi(&self) -> Result<OpenApi, PluginError> {
        use std::collections::HashSet;
        use utoipa::openapi::*;

        let mut combined_openapi = OpenApiBuilder::new()
            .info(
                InfoBuilder::new()
                    .title("Temps")
                    .description(Some(
                        "An API for managing projects, deployments, and infrastructure resources",
                    ))
                    .version("1.0.0")
                    .contact(Some(
                        ContactBuilder::new()
                            .name(Some("Temps Support"))
                            .url(Some("https://temps.sh"))
                            .build(),
                    ))
                    .build(),
            )
            .servers(Some(vec![ServerBuilder::new()
                .url("/api")
                .description(Some("Base path for all API endpoints"))
                .build()]))
            .components(Some(
                ComponentsBuilder::new()
                    .security_scheme("bearer_auth", self.create_bearer_auth_scheme())
                    .build(),
            ))
            .build();

        // Pass 1 — collect override-claimed paths and the plugin that claims
        // each. Plugins declare overrides through both `configure_routes`
        // and `configure_public_routes`; we union the two because the
        // OpenAPI doc is unified across listeners.
        let plugin_context = self.context.create_plugin_context();
        let mut path_owner_by_plugin: HashSet<(String /* plugin */, String /* path */)> =
            HashSet::new();
        for plugin in &self.plugins {
            if let Some(routes) = plugin.configure_routes(&plugin_context) {
                for ov in &routes.overrides {
                    path_owner_by_plugin.insert((plugin.name().to_string(), ov.path.clone()));
                }
            }
            if let Some(routes) = plugin.configure_public_routes(&plugin_context) {
                for ov in &routes.overrides {
                    path_owner_by_plugin.insert((plugin.name().to_string(), ov.path.clone()));
                }
            }
        }
        let overridden_paths: HashSet<String> = path_owner_by_plugin
            .iter()
            .map(|(_p, path)| path.clone())
            .collect();

        // Pass 2 — merge non-overridden paths normally. Any path in
        // `overridden_paths` from a plugin that didn't claim it is dropped
        // here; pass 3 puts the claimant's version in.
        for plugin in &self.plugins {
            let plugin_name = plugin.name().to_string();
            if let Some(plugin_openapi) = plugin.openapi_schema() {
                debug!("Merging OpenAPI schema for plugin: {}", plugin.name());
                let filtered = filter_overridden_paths(
                    plugin_openapi,
                    &overridden_paths,
                    &plugin_name,
                    &path_owner_by_plugin,
                );
                combined_openapi = self.merge_openapi_schemas(combined_openapi, filtered)?;
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

    /// Apply collected middleware to a router. Exposed so callers that merge
    /// extra (non-plugin) routes onto the admin listener can re-apply the
    /// same middleware stack — `Router::merge` doesn't propagate the parent
    /// router's layers to merged-in routes.
    pub fn apply_middleware_to_router(
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
            apply_to_public: false,
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

    /// Create a CORS layer using `tower_http::cors::CorsLayer`.
    ///
    /// # Example
    /// ```rust,no_run
    /// use tower_http::cors::{CorsLayer, Any};
    /// use axum::http::Method;
    ///
    /// let cors = CorsLayer::new()
    ///     .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
    ///     .allow_headers(Any)
    ///     .allow_origin(Any);  // or use .allow_origin("https://example.com".parse::<HeaderValue>().unwrap())
    /// ```
    ///
    /// Apply the layer directly to your Axum `Router`:
    /// ```rust,no_run
    /// router.layer(cors)
    /// ```
    pub fn cors_layer(allowed_origins: Vec<String>) -> tower_http::cors::CorsLayer {
        use axum::http::{HeaderValue, Method};
        use tower_http::cors::CorsLayer;

        let mut layer = CorsLayer::new().allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ]);

        if allowed_origins.iter().any(|o| o == "*") {
            layer = layer.allow_origin(tower_http::cors::Any);
        } else {
            let origins: Vec<HeaderValue> = allowed_origins
                .iter()
                .filter_map(|o| o.parse::<HeaderValue>().ok())
                .collect();
            layer = layer.allow_origin(origins);
        }

        layer
    }
}

#[cfg(test)]
mod split_application_tests {
    use super::*;
    use axum::routing::get;
    use std::future::Future;
    use std::pin::Pin;

    /// Plugin that registers a known admin handler under `/admin-marker` and
    /// a known public handler under `/public-marker`. Used to assert routes
    /// land on the correct side of [`PluginManager::build_split_application`].
    struct MarkerPlugin;

    impl TempsPlugin for MarkerPlugin {
        fn name(&self) -> &'static str {
            "marker"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route("/admin-marker", get(|| async { "admin" }));
            Some(PluginRoutes::new(router))
        }

        fn configure_public_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route("/public-marker", get(|| async { "public" }));
            Some(PluginRoutes::new(router))
        }
    }

    /// Probe an axum::Router with an in-memory oneshot request and return the
    /// response status. Avoids spinning up a real listener.
    async fn probe_status(router: Router, path: &str) -> axum::http::StatusCode {
        use tower::ServiceExt;
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status()
    }

    #[tokio::test]
    async fn split_application_routes_admin_only_to_admin() {
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(MarkerPlugin));

        let split = manager.build_split_application().unwrap();

        assert_eq!(
            probe_status(split.admin.clone(), "/admin-marker").await,
            axum::http::StatusCode::OK
        );
        assert_eq!(
            probe_status(split.public.clone(), "/admin-marker").await,
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn split_application_routes_public_only_to_public() {
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(MarkerPlugin));

        let split = manager.build_split_application().unwrap();

        assert_eq!(
            probe_status(split.public.clone(), "/public-marker").await,
            axum::http::StatusCode::OK
        );
        assert_eq!(
            probe_status(split.admin.clone(), "/public-marker").await,
            axum::http::StatusCode::NOT_FOUND
        );
    }

    /// Plugin used to verify that shared middleware (`apply_to_public = true`)
    /// is applied to both the admin and public routers, while admin-only
    /// middleware stays off the public router. Replicates the original bug:
    /// a public ingest handler that extracts `Extension<RequestMetadata>`
    /// returned HTTP 500 because no middleware injected the extension on
    /// the public side.
    struct MetadataRequiringPlugin;

    impl TempsPlugin for MetadataRequiringPlugin {
        fn name(&self) -> &'static str {
            "metadata-requiring"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route(
                "/admin-needs-metadata",
                get(
                    |axum::Extension(meta): axum::Extension<crate::RequestMetadata>| async move {
                        meta.host
                    },
                ),
            );
            Some(PluginRoutes::new(router))
        }

        fn configure_public_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route(
                "/public-needs-metadata",
                get(
                    |axum::Extension(meta): axum::Extension<crate::RequestMetadata>| async move {
                        meta.host
                    },
                ),
            );
            Some(PluginRoutes::new(router))
        }

        fn configure_middleware(&self, _ctx: &PluginContext) -> Option<PluginMiddlewareCollection> {
            let mut collection = PluginMiddlewareCollection::new();
            let key = [9u8; 32];
            let crypto = std::sync::Arc::new(crate::CookieCrypto::from_bytes(&key));
            collection.add_temps_middleware(std::sync::Arc::new(
                crate::RequestMetadataMiddleware::new(crypto),
            ));
            Some(collection)
        }
    }

    /// Plugin used to assert that middleware NOT opted into `apply_to_public`
    /// stays off the public router. Adds an admin-only middleware that
    /// short-circuits with HTTP 418 so we can detect whether it ran.
    struct AdminOnlyShortCircuitPlugin;

    impl TempsPlugin for AdminOnlyShortCircuitPlugin {
        fn name(&self) -> &'static str {
            "admin-only-shortcircuit"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route("/admin-probe", get(|| async { "admin-probe-ok" }));
            Some(PluginRoutes::new(router))
        }

        fn configure_public_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new().route("/public-probe", get(|| async { "public-probe-ok" }));
            Some(PluginRoutes::new(router))
        }

        fn configure_middleware(&self, _ctx: &PluginContext) -> Option<PluginMiddlewareCollection> {
            let mut collection = PluginMiddlewareCollection::new();
            // Plain `add_simple_middleware` -> `apply_to_public = false`. If
            // the partition logic ever regresses and applies this to the
            // public router, the probe will return 418 instead of 200.
            collection.add_simple_middleware(
                "shortcircuit",
                "admin-only-shortcircuit",
                MiddlewarePriority::Business,
                |_req: Request, _next: Next| async move {
                    Ok(axum::response::Response::builder()
                        .status(axum::http::StatusCode::IM_A_TEAPOT)
                        .body(axum::body::Body::empty())
                        .unwrap())
                },
            );
            Some(collection)
        }
    }

    #[tokio::test]
    async fn shared_middleware_applies_to_public_router() {
        // Regression: the public ingest endpoint `/api/_temps/session-replay/init`
        // failed with "Missing request extension RequestMetadata" because
        // the public router got no middleware. This test pins the wiring:
        // a public route that extracts `Extension<RequestMetadata>` must
        // return 200, not 500.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(MetadataRequiringPlugin));

        let split = manager.build_split_application().unwrap();

        assert_eq!(
            probe_status(split.public.clone(), "/public-needs-metadata").await,
            axum::http::StatusCode::OK,
            "public route extracting RequestMetadata must succeed — \
             RequestMetadataMiddleware must run on the public router"
        );
        assert_eq!(
            probe_status(split.admin.clone(), "/admin-needs-metadata").await,
            axum::http::StatusCode::OK,
            "admin route extracting RequestMetadata must succeed"
        );
    }

    #[tokio::test]
    async fn admin_only_middleware_does_not_apply_to_public_router() {
        // The flip side of the bug: middleware that doesn't opt into
        // `apply_to_public` (e.g. auth) must NOT run on public routes.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(AdminOnlyShortCircuitPlugin));

        let split = manager.build_split_application().unwrap();

        assert_eq!(
            probe_status(split.admin.clone(), "/admin-probe").await,
            axum::http::StatusCode::IM_A_TEAPOT,
            "admin-only middleware should run on the admin router"
        );
        assert_eq!(
            probe_status(split.public.clone(), "/public-probe").await,
            axum::http::StatusCode::OK,
            "admin-only middleware must NOT run on the public router"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Route override tests (PluginRoutes::with_override)
    // ──────────────────────────────────────────────────────────────────

    use axum::http::Method;
    use axum::response::IntoResponse;
    use axum::routing::post;

    /// Stand-in for an OSS plugin that owns `POST /auth/login` and
    /// `POST /auth/logout`. Returns 200 with a body the test can grep for.
    struct OssAuthLikePlugin;

    impl TempsPlugin for OssAuthLikePlugin {
        fn name(&self) -> &'static str {
            "oss-auth-like"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let router = Router::new()
                .route("/auth/login", post(|| async { "oss-login" }))
                .route("/auth/logout", post(|| async { "oss-logout" }));
            Some(PluginRoutes::new(router))
        }
    }

    /// EE plugin that overrides `POST /auth/login` with a 403. Does NOT
    /// touch `/auth/logout` — that one must keep flowing to the OSS handler.
    struct EeOverrideLoginPlugin;

    impl TempsPlugin for EeOverrideLoginPlugin {
        fn name(&self) -> &'static str {
            "ee-override-login"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let routes = PluginRoutes::new(Router::new()).with_override(
                Method::POST,
                "/auth/login",
                |_req: Request| async move {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        "ee-password-login-disabled",
                    )
                        .into_response()
                },
            );
            Some(routes)
        }
    }

    /// Second EE plugin that ALSO claims `POST /auth/login`. Used to
    /// verify last-loaded-wins collision policy.
    struct EeSecondLoginClaimantPlugin;

    impl TempsPlugin for EeSecondLoginClaimantPlugin {
        fn name(&self) -> &'static str {
            "ee-second-claimant"
        }

        fn register_services<'a>(
            &'a self,
            _ctx: &'a ServiceRegistrationContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn configure_routes(&self, _ctx: &PluginContext) -> Option<PluginRoutes> {
            let routes = PluginRoutes::new(Router::new()).with_override(
                Method::POST,
                "/auth/login",
                |_req: Request| async move {
                    (axum::http::StatusCode::IM_A_TEAPOT, "second-claimant").into_response()
                },
            );
            Some(routes)
        }
    }

    /// Probe an axum::Router with a POST request and return (status, body).
    async fn probe_post(router: Router, path: &str) -> (axum::http::StatusCode, String) {
        use tower::ServiceExt;
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn override_replaces_additive_handler_for_same_method_and_path() {
        // OSS registers POST /auth/login; EE overrides it. Override wins.
        // Without the override mechanism, Router::merge would panic on the
        // duplicate (Method, Path) pair — the override layer dispatches
        // before the inner router even sees the request.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(OssAuthLikePlugin));
        manager.register_plugin(Box::new(EeOverrideLoginPlugin));

        let split = manager.build_split_application().unwrap();
        let (status, body) = probe_post(split.admin.clone(), "/auth/login").await;

        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(body, "ee-password-login-disabled");
    }

    #[tokio::test]
    async fn non_overridden_routes_from_same_plugin_still_work() {
        // The override on POST /auth/login must not affect POST /auth/logout,
        // which OSS still owns. This is the critical "additive + selective
        // override" guarantee.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(OssAuthLikePlugin));
        manager.register_plugin(Box::new(EeOverrideLoginPlugin));

        let split = manager.build_split_application().unwrap();
        let (status, body) = probe_post(split.admin.clone(), "/auth/logout").await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, "oss-logout");
    }

    #[tokio::test]
    async fn override_does_not_leak_across_admin_public_listeners() {
        // An override declared in configure_routes (admin) must not affect
        // the public router. Same path on the public side keeps falling
        // through to whatever (if anything) the public router has —
        // here, nothing, so 404.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(OssAuthLikePlugin));
        manager.register_plugin(Box::new(EeOverrideLoginPlugin));

        let split = manager.build_split_application().unwrap();
        let (status, _) = probe_post(split.public.clone(), "/auth/login").await;

        assert_eq!(
            status,
            axum::http::StatusCode::NOT_FOUND,
            "admin-side override must not be visible on the public listener"
        );
    }

    #[tokio::test]
    async fn last_registered_override_wins_on_collision() {
        // Two EE plugins both claim POST /auth/login. Last-loaded wins —
        // documented policy, deterministic for the test. The collision is
        // logged via tracing::warn but the build still succeeds.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(OssAuthLikePlugin));
        manager.register_plugin(Box::new(EeOverrideLoginPlugin)); // first claimant
        manager.register_plugin(Box::new(EeSecondLoginClaimantPlugin)); // wins

        let split = manager.build_split_application().unwrap();
        let (status, body) = probe_post(split.admin.clone(), "/auth/login").await;

        assert_eq!(status, axum::http::StatusCode::IM_A_TEAPOT);
        assert_eq!(body, "second-claimant");
    }

    #[tokio::test]
    async fn override_runs_through_admin_middleware_stack() {
        // The override layer is applied *inside* the router, before
        // middleware is layered on top. So admin-side middleware (auth,
        // request-metadata, audit, etc.) still wraps overrides — they're
        // real routes from the lifecycle's perspective.
        //
        // Concretely: AdminOnlyShortCircuitPlugin's middleware returns 418
        // for every admin request. If middleware wraps the override layer
        // correctly, the override handler is never reached and 418 wins.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(OssAuthLikePlugin));
        manager.register_plugin(Box::new(EeOverrideLoginPlugin));
        manager.register_plugin(Box::new(AdminOnlyShortCircuitPlugin));

        let split = manager.build_split_application().unwrap();
        let (status, _) = probe_post(split.admin.clone(), "/auth/login").await;

        assert_eq!(
            status,
            axum::http::StatusCode::IM_A_TEAPOT,
            "middleware must still wrap overridden routes; 418 short-circuits before override handler runs"
        );
    }

    #[tokio::test]
    async fn plugin_routes_new_is_backwards_compatible() {
        // The 20+ existing plugins call PluginRoutes::new(router) with no
        // overrides. That must keep working — overrides default to empty,
        // and a no-override PluginManager must produce the same router shape
        // it did before the override mechanism existed.
        let mut manager = PluginManager::default();
        manager.register_plugin(Box::new(MarkerPlugin));

        let split = manager.build_split_application().unwrap();

        assert_eq!(
            probe_status(split.admin.clone(), "/admin-marker").await,
            axum::http::StatusCode::OK
        );
    }
}
