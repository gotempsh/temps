//! providers services and utilities

pub mod env_vars_provider_impl;
pub mod externalsvc;
pub mod health_monitor;
pub mod parameter_strategies;
pub mod postgres_lifecycle;
pub mod postgres_upgrade_service;
pub mod query_service;
pub mod remote_service_client;
pub mod services;
pub use services::*;
pub mod plugin;
mod types;
mod utils;
pub use externalsvc::ClusterRole;
pub use externalsvc::PgAutoFailoverState;
pub use externalsvc::S3Credentials;
pub use externalsvc::ServiceType;
pub use query_service::QueryService;

// Re-export `DnsRegistry` so downstream callers that construct
// `ExternalServiceManager` don't need a separate `temps-dns` dependency.
// The registry is required by `ExternalServiceManager::new`.
pub use temps_dns::DnsRegistry;

pub mod handlers;

// Export plugin
pub use env_vars_provider_impl::ExternalServicesEnvProvider;
pub use plugin::ProvidersPlugin;
