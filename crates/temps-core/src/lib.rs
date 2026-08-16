//! Core utilities and types shared across all Temps crates

pub mod admin_gate;
pub mod ai_tool_call;
pub mod audit;
pub mod client_ip;
pub mod config;
pub mod deployment;
pub mod dns_automation;
pub mod env_vars_provider;
pub mod error;
pub mod error_builder;
pub mod error_metrics;
pub mod external_plugin;
pub mod feature_maturity;
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
pub mod self_update;
pub mod sensitive_action;
pub mod telemetry;
pub mod time_window;
pub mod tls;
pub mod traces;
pub mod update_status;
pub use problemdetails::ProblemDetails;
pub use self_update::{
    ReleaseCheckResult, SelfUpdateAttempt, SelfUpdateBlocker, SelfUpdateCapability,
    SelfUpdateError, SelfUpdatePhase, SelfUpdatePolicy, SelfUpdateRestartMode, SelfUpdateStatus,
    SelfUpdater, StartedSelfUpdate, SupervisorKind, SELF_UPDATE_JOURNAL_FILE,
};
pub use update_status::{AvailableUpdate, UpdateStatusSlot, UPGRADE_DOCS_URL};
mod app_settings;
mod constants;
mod cookie_crypto;
#[allow(deprecated)] // generic-array 0.14.x deprecation in aes-gcm 0.10
pub mod ecies;
mod encryption;
pub mod preview_grant;
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
pub use client_ip::resolve_client_ip;
pub use config::*;
pub use constants::*;
pub use deployment::*;
pub use dns_automation::{
    DnsAutomationDecision, DnsAutomationError, DnsAutomationGate, DnsAutomationGateSlot,
    DnsAutomationMutation, DnsAutomationPurpose, DnsAutomationRequest,
};
pub use env_vars_provider::{
    flatten_integration_env_vars, IntegrationEnvVar, IntegrationServiceInfo,
    ProjectEnvVarsProvider, ProjectIntegrationEnvVars,
};
pub use error::*;
pub use error_builder::*;
pub use jobs::*;
pub use on_demand::*;
pub use project_access::{MembershipPermissionResolver, ProjectAccessChecker};
pub use public_hostname::{base_domain as public_base_domain, PublicHostnameStrategy};
pub use public_hostname_resolver::{
    match_strategy, PublicHostnameResolver, StandardHostnameResolver,
};
pub use retention::{
    FixedRetentionResolver, RetentionResolver, RetentionResolverSlot, RetentionTable,
};
pub use secrets_manager::SecretsManagerResolver;
pub use sensitive_action::{
    SensitiveAction, SensitiveActionAuthorizationError, SensitiveActionAuthorizer,
    SensitiveActionDecision, SensitiveActionPrincipal,
};
pub use telemetry::{NoopTelemetryReporter, TelemetryEvent, TelemetryEventKind, TelemetryReporter};
pub use traces::{
    TraceQueryFilter, TraceReader, TraceReaderError, TraceSpanDto, TraceSpanEventDto,
    TraceSummaryDto,
};
pub use utils::*;

// Re-export external dependencies
pub use anyhow;
pub use app_settings::{
    AgentSandboxSettings, AiChatLimitsSettings, AiConfigSettings, AppSettings, BuildLimitsSettings,
    ClusterDnsSettings, ConnectionLimitSettings, ContainerLogSettings, DiskSpaceAlertSettings,
    DnsProviderSettings, DockerRegistrySettings, LetsEncryptSettings, MetricsStoreKind,
    MonitoringSettings, MultiNodeSettings, ObservabilityCompressionSettings,
    ObservabilityRetentionSettings, PreviewGatewaySettings, ProviderConfig, RateLimitSettings,
    RequestTimeoutSettings, ScreenshotSettings, SecurityHeadersSettings, SelfUpdateSettings,
};
pub use async_trait;
pub use chrono;
pub use cookie_crypto::{CookieCrypto, CryptoError};
pub use encryption::EncryptionService;
pub use preview_grant::{
    encode_preview_session_grant, sanitize_preview_next, validate_preview_session_grant_envelope,
    verify_preview_session_grant, PreviewGrantError, PREVIEW_SESSION_GRANT_MAX_TTL,
    PREVIEW_SESSION_GRANT_TTL, PREVIEW_SESSION_GRANT_VERSION,
};
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
pub mod archive_security;
