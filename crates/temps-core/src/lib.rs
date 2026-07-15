//! Core utilities and types shared across all Temps crates

pub mod admin_gate;
pub mod audit;
pub mod config;
pub mod deployment;
pub mod env_vars_provider;
pub mod error;
pub mod error_builder;
pub mod external_plugin;
pub mod jobs;
pub mod node_pki;
pub mod notifications;
pub mod on_demand;
pub mod openapi;
pub mod plugin;
pub mod problemdetails;
pub mod project_access;
pub mod public_hostname;
pub mod public_hostname_resolver;
pub mod retention;
pub mod retry;
pub mod secrets_manager;
pub mod telemetry;
pub mod tls;
pub mod traces;
pub use problemdetails::ProblemDetails;
mod app_settings;
mod constants;
mod cookie_crypto;
#[allow(deprecated)] // generic-array 0.14.x deprecation in aes-gcm 0.10
pub mod ecies;
mod encryption;
pub mod repo_config;
mod request_metadata;
pub mod route_table;
pub mod stages;
pub mod templates;
pub mod types;
pub mod url_validation;
pub mod utils;
pub mod workflow;
pub mod workflow_executor;
pub mod workflow_memory;
// Re-export commonly used types
pub use audit::*;
pub use config::*;
pub use constants::*;
pub use deployment::*;
pub use env_vars_provider::{
    flatten_integration_env_vars, IntegrationEnvVar, IntegrationServiceInfo,
    ProjectEnvVarsProvider, ProjectIntegrationEnvVars,
};
pub use error::*;
pub use error_builder::*;
pub use jobs::*;
pub use on_demand::*;
pub use project_access::ProjectAccessChecker;
pub use public_hostname::{base_domain as public_base_domain, PublicHostnameStrategy};
pub use public_hostname_resolver::{
    match_strategy, PublicHostnameResolver, StandardHostnameResolver,
};
pub use retention::{
    FixedRetentionResolver, RetentionResolver, RetentionResolverSlot, RetentionTable,
};
pub use secrets_manager::SecretsManagerResolver;
pub use telemetry::{NoopTelemetryReporter, TelemetryEvent, TelemetryEventKind, TelemetryReporter};
pub use traces::{
    TraceQueryFilter, TraceReader, TraceReaderError, TraceSpanDto, TraceSpanEventDto,
    TraceSummaryDto,
};
pub use utils::*;

// Re-export external dependencies
pub use anyhow;
pub use app_settings::{
    AgentSandboxSettings, AiConfigSettings, AppSettings, BuildLimitsSettings, ClusterDnsSettings,
    ContainerLogSettings, DiskSpaceAlertSettings, DnsProviderSettings, DockerRegistrySettings,
    LetsEncryptSettings, MetricsStoreKind, MonitoringSettings, MultiNodeSettings,
    PreviewGatewaySettings, ProviderConfig, RateLimitSettings, ScreenshotSettings,
    SecurityHeadersSettings,
};
pub use async_trait;
pub use chrono;
pub use cookie_crypto::{CookieCrypto, CryptoError};
pub use encryption::EncryptionService;
pub use repo_config::*;
pub use request_metadata::{
    build_from_request as build_request_metadata, host_without_port, request_metadata_middleware,
    RequestMetadata, RequestMetadataMiddleware,
};
pub use serde;
pub use serde_json;
pub use stages::*;
pub use templates::*;
pub use thiserror;
pub use tokio;
pub use tracing;
pub use types::*;
pub use uuid;
pub use workflow::*;
pub use workflow_executor::*;
pub use workflow_memory::{
    memory_install_command, WorkflowMemoryError, WorkflowMemoryFact, WorkflowMemoryProvider,
    MEMORY_SCRIPT, MEMORY_SCRIPT_DIR, MEMORY_SCRIPT_PATH,
};

// Re-export standard datetime type for use across all crates
pub use types::UtcDateTime;
