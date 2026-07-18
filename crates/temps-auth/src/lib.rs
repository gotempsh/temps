mod apikey_handler;
mod apikey_handler_types;
mod apikey_plugin;
mod apikey_service;
mod apikey_types;
mod audit;
mod auth_service;
mod avatar;
pub mod cli_auth_handler;
pub mod cli_device_handler;
pub mod context;
mod decorators;
mod deployment_token_service;
mod email_templates;
pub mod handlers;
mod last_used_throttle;
mod macros;
mod middleware;
mod oidc_errors;
mod oidc_handler;
mod oidc_service;
mod oidc_types;
mod permission_attribute;
mod permission_decorator;
mod permission_guard;
pub mod permissions;
mod plugin;
pub mod rate_limit;
pub mod state;
mod temps_middleware;
mod types;
mod user_service;

pub use decorators::*;
pub use macros::*;
pub use middleware::*;
pub use permission_attribute::*;
pub use temps_core::resolve_client_ip;

pub use context::*;
pub use permissions::*;
pub use state::*;

// Export plugins
pub use apikey_plugin::ApiKeyPlugin;
pub use plugin::AuthPlugin;

// Export services
pub use apikey_service::{ApiKeyService, CreateApiKeyRequest, CreateApiKeyResponse};
pub use auth_service::{validate_password_complexity, AuthService};
pub use deployment_token_service::{
    DeploymentTokenValidationError, DeploymentTokenValidationService, ValidatedDeploymentToken,
};
pub use user_service::UserService;

// Export TempsMiddleware implementation
pub use temps_middleware::AuthMiddleware;
