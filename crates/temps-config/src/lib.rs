// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod app_settings;
pub mod cluster_ca;
pub mod deployment_config;
pub mod disk_status;
pub mod enrollment_tokens;
mod installation_secrets;
pub mod loopback_reservation;
pub mod node_client;
pub mod otel_export;
pub mod public_ingress;
mod retention;
mod service;

pub use app_settings::{
    decide_env_setting_migration, decide_secret_env_migration, normalize_loopback_ja4_header,
    record_env_setting_persisted, AgentSandboxSettings, AiChatLimitsSettings, AiConfigSettings,
    AiWorkspaceFileLimitsSettings, AnalyticsRetentionSettings, AppSettings, BackupExecutorSettings,
    BlobSettings, BuildLimitsSettings, CeilingEnforcement, ClickHouseSettings, CloudSettings,
    ClusterDnsSettings, ConnectionLimitSettings, ContainerLogSettings,
    ControlPlaneResourceSettings, DiskSpaceAlertSettings, DnsProviderSettings,
    DockerRegistrySettings, EnvMigrationLedger, EnvSecret, EnvSettingMigratedAudit,
    EnvSettingMigration, GeoLicenseKeyIntent, GeoSettings, GeoSettingsError,
    ImageRetentionSettings, KvSettings, LetsEncryptSettings, LoopbackJa4HeaderError,
    McpServerSettings, MetricsStoreKind, MonitoringSettings, MultiNodeSettings,
    ObservabilityCompressionSettings, OnDemandTlsSettings, OtelIngestSettings,
    OtelIngestSettingsError, OtelS3ArchiveSettings, OtelS3Credentials, PreviewGatewaySettings,
    ProviderConfig, RateLimitSettings, RequestTimeoutSettings, ScreenshotSettings,
    SecurityHeadersSettings, SelfUpdateSettings, ShieldSettings, TenantResourceCeilings,
    DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, DEFAULT_BACKUP_EXECUTOR_MAX_CONCURRENT,
    DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR, DEFAULT_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES,
    DEFAULT_GEO_REFRESH_INTERVAL_HOURS, DEFAULT_GEO_STALE_LOOKUP_DAYS,
    DEFAULT_OTEL_RATE_LIMIT_REQUESTS, DEFAULT_OTEL_RATE_LIMIT_WINDOW_SECS,
    DEFAULT_OTEL_S3_ARCHIVE_PREFIX, DEFAULT_UNREFERENCED_IP_RETENTION_DAYS,
    ENV_MIGRATION_LEDGER_KEY, GEO_CHECK_STATUS_ERROR, GEO_CHECK_STATUS_OK,
    GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY, GEO_SOURCE_BUNDLED_GITHUB,
    GEO_SOURCE_MAXMIND_OFFICIAL, IP_REFERENCE_SLACK_DAYS, MAX_BACKUP_EXECUTOR_MAX_CONCURRENT,
    MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR, MAX_GEO_REFRESH_INTERVAL_HOURS,
    MAX_GEO_STALE_LOOKUP_DAYS, MAX_SCRAPE_INTERVAL_SECS, MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
    MIN_CLOUD_TELEMETRY_OUTBOX_MAX_BYTES, MIN_GEO_REFRESH_INTERVAL_HOURS,
    MIN_GEO_STALE_LOOKUP_DAYS,
};
pub use disk_status::{
    collect_disk_status, disk_for_path, get_disk_info, DiskInfo, DiskSpaceAlert,
    DiskSpaceCheckResult, DiskStatusError,
};
pub use enrollment_tokens::{EnrollmentError, EnrollmentTokenService, MintParams};
pub use installation_secrets::{
    bootstrap_stateless_requested, resolve_installation_secrets, InstallationSecrets,
    AUTH_SECRET_ENV, AUTH_SECRET_FILE_ENV, ENCRYPTION_KEY_ENV, ENCRYPTION_KEY_FILE_ENV,
    STATELESS_ENV,
};
pub use retention::{settings_retention_resolver, RETENTION_REFRESH_INTERVAL};
pub use service::{
    anonymous_telemetry_preference, installation_mode, stateless_instance_id,
    stateless_telemetry_anonymous_id, stored_clickhouse_compression, ClusterCaRotationResult,
    ClusterNetworkState, ConfigService, ConfigServiceError, EffectiveTelemetryPolicies,
    InstallationMode, RetentionPolicy, ServerConfig, SettingsUpdateResult,
    TrustedPrivateNetworksIntent,
};
// Used by the settings handlers in `temps-config-handlers`.
pub use service::preserve_provider_credential_proof;
