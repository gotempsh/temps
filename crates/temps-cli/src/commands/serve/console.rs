// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use argon2::{Argon2, PasswordHasher};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Router;
use chrono;
use colored::Colorize;
use futures::FutureExt;
use include_dir::{include_dir, Dir};
use rand::RngExt;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_agents::AgentsPlugin;
use temps_analytics::AnalyticsPlugin;
use temps_analytics_events::EventsPlugin;
use temps_analytics_funnels::FunnelsPlugin;
use temps_analytics_performance::PerformancePlugin;
use temps_analytics_session_replay::SessionReplayPlugin;
use temps_audit::AuditPlugin;
use temps_auth::{ApiKeyPlugin, AuthPlugin};
use temps_backup::BackupPlugin;
use temps_blob::BlobPlugin;
use temps_cloud::{
    CloudEnrollmentActor, CloudPlugin, CloudService, CloudServiceError, ManagedBackupOutcome,
};
use temps_cloud_client::FirstLinkEnrollment;
use temps_config::ConfigPlugin;
use temps_config::ServerConfig;
use temps_core::plugin::{PluginManager, TempsPlugin};
use temps_telemetry::TelemetryPlugin;
// `TempsPlugin` is used both directly (extra_plugins field) and through
// `dyn TempsPlugin` in `ConsoleApiParams`.
use temps_core::templates::TemplateService;
use temps_core::{CookieCrypto, EncryptionService};
use temps_database::DbConnection;
use temps_deployer::plugin::DeployerPlugin;
use temps_deployer::traefik_discovery::DriftAlarmSink;
use temps_deployments::DeploymentsPlugin;
use temps_dns::DnsPlugin;
use temps_domains::DomainsPlugin;
use temps_email::EmailPlugin;
use temps_entities::users;
use temps_environments::EnvironmentsPlugin;
use temps_error_tracking::ErrorTrackingPlugin;
use temps_flags::FlagsPlugin;
use temps_geo::GeoPlugin;
use temps_git::GitPlugin;
use temps_import::ImportPlugin;
use temps_infra::InfraPlugin;
use temps_kv::KvPlugin;
use temps_log_aggregator::{LogAggregatorPlugin, StorageConfig};
use temps_logs::LogsPlugin;
use temps_mcp_server::{McpHandlerState, McpServerPlugin};
use temps_monitoring::{
    AlarmService, ContainerHealthConfig, ContainerHealthMonitor, DiskSpaceMonitor,
    MonitoringPlugin, OutageDetectionService,
};
use temps_notifications::NotificationsPlugin;
use temps_observability::ObservabilityPlugin;
use temps_otel::plugin::OtelPlugin;
use temps_projects::ProjectsPlugin;
use temps_providers::ProvidersPlugin;
use temps_proxy::ProxyPlugin;
use temps_queue::QueuePlugin;
use temps_revenue::RevenuePlugin;
use temps_sandbox::plugin::SandboxPlugin;
use temps_screenshots::ScreenshotsPlugin;
use temps_static_files::StaticFilesPlugin;
use temps_status_page::StatusPagePlugin;
use temps_teams::TeamsPlugin;
use temps_vulnerability_scanner::VulnerabilityScannerPlugin;
use temps_webhooks::WebhooksPlugin;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

// Multi-node support
use temps_deployments::handlers::nodes::NodeAppState;
use temps_deployments::jobs::node_health_check::{
    check_control_plane_resources, check_drain_completion, check_node_health, check_node_resources,
    failover_offline_nodes, notify_nodes_offline, refresh_control_plane_metrics,
};
use temps_deployments::services::node_service::NodeService;
use utoipa_swagger_ui::SwaggerUi;

// Embed the dist directory at compile time
static WEBSITE: Dir = include_dir!("$CARGO_MANIFEST_DIR/dist");

/// Optional replacement UI bundle supplied by an embedding binary (EE,
/// VibeTemps, ...) via [`crate::set_embedded_ui`] before `dispatch`. When
/// set, the SPA fallback serves this bundle at the document root instead of
/// the OSS console. The OSS console's API surface is unaffected.
static WEBSITE_OVERRIDE: std::sync::OnceLock<&'static Dir<'static>> = std::sync::OnceLock::new();

pub(crate) fn set_embedded_ui(dir: &'static Dir<'static>) -> Result<(), &'static str> {
    WEBSITE_OVERRIDE
        .set(dir)
        .map_err(|_| "embedded UI override already set")
}

/// Optional extra listener that serves the ORIGINAL temps console (admin API
/// and original SPA bundle) when the document root has been overridden by an
/// embedding binary. A separate listener — not a path prefix — because the
/// console SPA assumes it owns its origin (absolute asset paths, client-side
/// routing). Bind to loopback unless you mean to expose it.
static PLATFORM_CONSOLE_ADDR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn set_platform_console_addr(addr: String) -> Result<(), &'static str> {
    PLATFORM_CONSOLE_ADDR
        .set(addr)
        .map_err(|_| "platform console address already set")
}

fn website() -> &'static Dir<'static> {
    WEBSITE_OVERRIDE.get().copied().unwrap_or(&WEBSITE)
}

/// Ensure the system user (id=0) exists in the database.
/// Emit the anonymous `instance_started` telemetry event with non-identifying
/// depth-of-usage counts (number of projects, environments, managed services,
/// and worker nodes). These counts are a strong retention signal without
/// revealing anything about *what* the operator is running.
///
/// Fully best-effort: any count query failure is swallowed (the count is simply
/// omitted) and `report()` itself is fire-and-forget.
async fn report_instance_started(
    reporter: &dyn temps_core::telemetry::TelemetryReporter,
    db: &sea_orm::DatabaseConnection,
) {
    use temps_core::telemetry::TelemetryEventKind;
    reporter.report(build_instance_event(TelemetryEventKind::InstanceStarted, db).await);
}

/// Coarse, non-identifying RAM capacity band for the host. We deliberately
/// bucket (rather than send exact byte counts) so the value can't contribute to
/// fingerprinting: it answers "are people running Temps on tiny VPSes vs beefy
/// boxes?" without revealing the machine's real specs. Returns `None` if the
/// total can't be read.
fn capacity_tier_from_total_ram() -> Option<&'static str> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    // sysinfo reports total_memory() in bytes.
    let total_bytes = sys.total_memory();
    if total_bytes == 0 {
        return None;
    }
    let gib = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    // Bands chosen around common VPS sizes; coarse on purpose.
    let tier = if gib < 1.5 {
        "xs" // ~1 GiB and under
    } else if gib < 3.0 {
        "small" // ~2 GiB
    } else if gib < 6.0 {
        "medium" // ~4 GiB
    } else if gib < 12.0 {
        "large" // ~8 GiB
    } else if gib < 24.0 {
        "xl" // ~16 GiB
    } else {
        "xxl" // 24 GiB+
    };
    Some(tier)
}

/// Build an instance lifecycle/heartbeat event carrying a small set of
/// non-identifying signals:
/// - depth-of-usage counts (projects, environments, managed services, worker
///   nodes),
/// - `has_git_provider`: whether the instance has wired up at least one git
///   provider connection (a key activation signal — git-push deploys are the
///   core workflow),
/// - `capacity_tier`: a COARSE RAM band (never exact specs; see
///   [`capacity_tier_from_total_ram`]).
///
/// Shared by `instance_started` and the periodic `instance_heartbeat` so both
/// report the fleet snapshot identically. Each field is independent and
/// optional — a failure on one doesn't block the others or the event itself.
async fn build_instance_event(
    kind: temps_core::telemetry::TelemetryEventKind,
    db: &sea_orm::DatabaseConnection,
) -> temps_core::telemetry::TelemetryEvent {
    use sea_orm::PaginatorTrait;
    use temps_core::telemetry::TelemetryEvent;

    let project_count = temps_entities::projects::Entity::find()
        .count(db)
        .await
        .ok();
    let environment_count = temps_entities::environments::Entity::find()
        .count(db)
        .await
        .ok();
    let service_count = temps_entities::external_services::Entity::find()
        .count(db)
        .await
        .ok();
    let node_count = temps_entities::nodes::Entity::find().count(db).await.ok();

    // Whether git is configured on this instance at all (>= 1 provider
    // connection). Just a boolean — no provider type, no URLs, no tokens.
    let has_git_provider = temps_entities::git_provider_connections::Entity::find()
        .count(db)
        .await
        .ok()
        .map(|c| c > 0);

    let capacity_tier = capacity_tier_from_total_ram();

    TelemetryEvent::new(kind)
        .with_opt("project_count", project_count.map(|c| c as i64))
        .with_opt("environment_count", environment_count.map(|c| c as i64))
        .with_opt("service_count", service_count.map(|c| c as i64))
        .with_opt("node_count", node_count.map(|c| c as i64))
        .with_opt("has_git_provider", has_git_provider)
        .with_opt("capacity_tier", capacity_tier)
}

/// Interval between anonymous `instance_heartbeat` events. Daily — the minimum
/// grain that keeps "active instances" (which is bucketed per day) accurate, so
/// a live-but-idle instance still registers as active each day it's running.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Spawn a detached task that emits an anonymous `instance_heartbeat` once per
/// [`HEARTBEAT_INTERVAL`] for as long as the server runs. This is what makes the
/// "active instances" metric mean "alive" rather than merely "did something" —
/// an instance that isn't deploying today still checks in.
///
/// The very first heartbeat fires after one interval (the `instance_started`
/// event already covers "active today" at boot, so we don't double-send on
/// startup). Fully best-effort and respects opt-out: a disabled reporter makes
/// `report()` a no-op, and a dead endpoint never affects the server.
fn spawn_heartbeat_task(
    reporter: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
    db: std::sync::Arc<sea_orm::DatabaseConnection>,
) {
    use temps_core::telemetry::TelemetryEventKind;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        // The first tick completes immediately; skip it so the first heartbeat
        // lands one full interval after boot (boot is already covered by
        // instance_started).
        interval.tick().await;
        loop {
            interval.tick().await;
            let event =
                build_instance_event(TelemetryEventKind::InstanceHeartbeat, db.as_ref()).await;
            reporter.report(event);
            tracing::debug!("emitted anonymous instance_heartbeat telemetry event");
        }
    });
}

/// Interval between anonymous `error_summary` flushes. Shorter than the daily
/// heartbeat so shorter-lived instances still report, but coarse enough that
/// even a melting-down instance costs at most 4 small POSTs per day.
const ERROR_SUMMARY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Spawn a detached task that drains the process-global error counters (see
/// `temps_core::error_metrics`) every [`ERROR_SUMMARY_INTERVAL`] and reports
/// one aggregated `error_summary` event. Emits nothing when no errors were
/// recorded, so healthy instances stay silent. Best-effort and opt-out aware
/// like every other telemetry emission.
fn spawn_error_summary_task(
    reporter: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(ERROR_SUMMARY_INTERVAL);
        // Skip the immediate first tick: nothing meaningful has accumulated
        // at boot, and instance_started already covers "alive today".
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(summary) = temps_core::error_metrics::global().drain() else {
                continue;
            };
            reporter.report(build_error_summary_event(&summary));
            tracing::debug!("emitted anonymous error_summary telemetry event");
        }
    });
}

/// Build the `error_summary` telemetry event from a drained counter snapshot.
///
/// Every value is a count or a compile-time identifier of our own code
/// (tracing target, route template, crate-relative source location) — see the
/// privacy contract in `temps_core::error_metrics`. `overflow` is included
/// only when non-zero so truncation by the key cap is never silent.
fn build_error_summary_event(
    summary: &temps_core::error_metrics::ErrorSummary,
) -> temps_core::telemetry::TelemetryEvent {
    use temps_core::telemetry::{TelemetryEvent, TelemetryEventKind};

    let mut event = TelemetryEvent::new(TelemetryEventKind::ErrorSummary)
        .with(
            "window_hours",
            (ERROR_SUMMARY_INTERVAL.as_secs() / 3600) as i64,
        )
        .with("total", summary.total as i64)
        .with_opt(
            "overflow",
            (summary.overflow > 0).then_some(summary.overflow as i64),
        );
    for (category, count) in &summary.category_totals {
        event = event.with(format!("{category}_total"), *count as i64);
    }
    let top: Vec<serde_json::Value> = summary
        .top
        .iter()
        .map(|entry| {
            serde_json::json!({
                "category": entry.category,
                "key": entry.key,
                "count": entry.count,
            })
        })
        .collect();
    event.with("top", serde_json::Value::Array(top))
}

/// Middleware counting console-API 5xx responses for the anonymous
/// `error_summary` telemetry event.
///
/// Records only the method, the route TEMPLATE (axum's `MatchedPath`, e.g.
/// `/api/projects/{id}` — never the concrete URL, query, or body), and the
/// status code. Unmatched requests (e.g. the SPA fallback) are recorded under
/// the fixed label `unmatched` so a 500 storm there is still visible without
/// capturing raw paths. Runs only on the console listeners — proxied user-app
/// traffic never passes through this router, so user requests are never
/// counted. Cost outside the 5xx case is one extension lookup and two short
/// string allocations per request (fine for the control plane; this
/// middleware must never be mounted on the proxy data path).
async fn track_server_errors(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let response = next.run(req).await;
    if response.status().is_server_error() {
        temps_core::error_metrics::record_http_5xx(
            &method,
            route.as_deref().unwrap_or("unmatched"),
            response.status().as_u16(),
        );
    }
    response
}

/// This user is referenced by webhook-created resources (e.g., GitHub App installations)
/// that don't have an authenticated user context.
async fn ensure_system_user(db: &sea_orm::DatabaseConnection) -> anyhow::Result<()> {
    let system_user_exists = users::Entity::find_by_id(0)
        .one(db)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to check system user: {}", e))?
        .is_some();

    if !system_user_exists {
        let now = chrono::Utc::now();
        let system_user = users::ActiveModel {
            id: Set(0),
            name: Set("System".to_string()),
            email: Set("system@localhost".to_string()),
            password_hash: Set(None),
            email_verified: Set(true),
            email_verification_token: Set(None),
            email_verification_expires: Set(None),
            password_reset_token: Set(None),
            password_reset_expires: Set(None),
            must_change_password: Set(false),
            deleted_at: Set(None),
            mfa_enabled: Set(false),
            mfa_secret: Set(None),
            mfa_recovery_codes: Set(None),
            oidc_subject: Set(None),
            oidc_provider_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };

        system_user
            .insert(db)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create system user: {}", e))?;
        debug!("Created system user (id=0)");
    }

    Ok(())
}

fn generate_secure_password() -> String {
    const CHARSET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
    let mut rng = rand::rng();
    (0..16)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

async fn create_initial_admin_user(
    conn: &sea_orm::DatabaseConnection,
    email: &str,
    configured_password: Option<&str>,
) -> Result<(), InitialAdminBootstrapError> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, QueryFilter, TransactionTrait};

    // Check if user with this email already exists (normalize to lowercase)
    let email_lower = email.to_lowercase();
    let existing_user = users::Entity::find()
        .filter(users::Column::Email.eq(&email_lower))
        .one(conn)
        .await
        .map_err(|source| InitialAdminBootstrapError::LookupUser {
            email: email_lower.clone(),
            source,
        })?;

    if let Some(existing_user) = existing_user {
        ensure_existing_initial_admin_is_active(existing_user.deleted_at.is_some(), &email_lower)?;
        info!("User with email {} already exists", email_lower);
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
        );
        println!(
            "{}",
            "   ⚠️  Admin account already exists!"
                .bright_yellow()
                .bold()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
        );
        println!();
        println!(
            "{} {}",
            "Email:".bright_white().bold(),
            email_lower.bright_cyan()
        );
        println!();
        println!(
            "{}",
            "This admin account was created previously.".bright_white()
        );
        println!(
            "{}",
            "If you forgot the password, use the reset command:".bright_white()
        );
        println!();
        println!(
            "  {} {}",
            "$".bright_cyan(),
            "temps reset-admin-password".bright_green()
        );
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
        );
        println!();
        return Ok(());
    }

    let password = configured_password
        .map(str::to_owned)
        .unwrap_or_else(generate_secure_password);

    // Hash the password using Argon2
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(password.as_bytes())
        .map_err(|error| InitialAdminBootstrapError::HashPassword {
            email: email_lower.clone(),
            reason: error.to_string(),
        })?
        .to_string();

    // Resolve the role before creating anything so a missing role cannot leave
    // partial bootstrap state.
    let admin_role = temps_entities::roles::Entity::find()
        .filter(temps_entities::roles::Column::Name.eq("admin"))
        .one(conn)
        .await
        .map_err(|source| InitialAdminBootstrapError::LookupAdminRole {
            email: email_lower.clone(),
            source,
        })?
        .ok_or_else(|| InitialAdminBootstrapError::AdminRoleNotFound {
            email: email_lower.clone(),
        })?;

    // Create the user and role assignment atomically. A partial bootstrap would
    // leave a non-deleted user that suppresses future bootstrap attempts but
    // cannot administer the instance.
    let transaction =
        conn.begin()
            .await
            .map_err(|source| InitialAdminBootstrapError::BeginTransaction {
                email: email_lower.clone(),
                source,
            })?;

    // Create the user with normalized email
    let new_user = users::ActiveModel {
        email: Set(email_lower.clone()),
        name: Set("Admin".to_string()),
        password_hash: Set(Some(password_hash)),
        email_verified: Set(true), // Admin email is verified since provided interactively
        mfa_enabled: Set(false),
        mfa_secret: Set(None),
        mfa_recovery_codes: Set(None),
        deleted_at: Set(None),
        email_verification_token: Set(None),
        email_verification_expires: Set(None),
        password_reset_token: Set(None),
        password_reset_expires: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    let user = new_user.insert(&transaction).await.map_err(|source| {
        InitialAdminBootstrapError::CreateUser {
            email: email_lower.clone(),
            source,
        }
    })?;

    // Assign admin role to the user
    let user_role = temps_entities::user_roles::ActiveModel {
        user_id: Set(user.id),
        role_id: Set(admin_role.id),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    user_role.insert(&transaction).await.map_err(|source| {
        InitialAdminBootstrapError::AssignAdminRole {
            email: email_lower.clone(),
            user_id: user.id,
            role_id: admin_role.id,
            source,
        }
    })?;
    transaction
        .commit()
        .await
        .map_err(|source| InitialAdminBootstrapError::CommitTransaction {
            email: email_lower.clone(),
            source,
        })?;

    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!(
        "{}",
        "   🎉 Admin account created successfully!"
            .bright_white()
            .bold()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!();
    println!(
        "{} {}",
        "Email:".bright_white().bold(),
        email_lower.bright_cyan()
    );
    if configured_password.is_none() {
        println!(
            "{} {}",
            "Password:".bright_white().bold(),
            password.bright_yellow().bold()
        );
        println!();
        println!(
            "{}",
            "⚠️  IMPORTANT: Save this password now!"
                .bright_yellow()
                .bold()
        );
        println!(
            "{}",
            "This is the only time it will be displayed.".bright_white()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!();

        // Interactive starts pause so the operator can save the generated password.
        loop {
            print!(
                "{} ",
                "Have you saved the password? (y/n):".bright_white().bold()
            );
            io::stdout().flush().map_err(|source| {
                InitialAdminBootstrapError::InteractivePrompt {
                    email: email_lower.clone(),
                    operation: "flush password confirmation prompt",
                    source,
                }
            })?;

            let mut response = String::new();
            io::stdin().read_line(&mut response).map_err(|source| {
                InitialAdminBootstrapError::InteractivePrompt {
                    email: email_lower.clone(),
                    operation: "read password confirmation",
                    source,
                }
            })?;
            let response = response.trim().to_lowercase();

            if response == "y" || response == "yes" {
                println!();
                println!("{}", "✅ Great! Starting the server...".bright_green());
                println!();
                break;
            } else if response == "n" || response == "no" {
                println!();
                println!(
                    "{}",
                    "Please save the password before continuing.".bright_yellow()
                );
                println!(
                    "{} {}",
                    "Password:".bright_white().bold(),
                    password.bright_yellow().bold()
                );
                println!();
            } else {
                println!(
                    "{}",
                    "Please enter 'y' for yes or 'n' for no.".bright_white()
                );
            }
        }
    } else {
        info!("Initial admin created from TEMPS_ADMIN_EMAIL and password secret file");
    }

    debug!("Created initial admin user with email: {}", email);

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum InitialAdminBootstrapError {
    #[error("failed to look up initial admin '{email}': {source}")]
    LookupUser {
        email: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("failed to hash password for initial admin '{email}': {reason}")]
    HashPassword { email: String, reason: String },
    #[error("failed to look up admin role while bootstrapping '{email}': {source}")]
    LookupAdminRole {
        email: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("admin role not found while bootstrapping initial admin '{email}'")]
    AdminRoleNotFound { email: String },
    #[error("failed to begin initial-admin transaction for '{email}': {source}")]
    BeginTransaction {
        email: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("failed to create initial admin user '{email}': {source}")]
    CreateUser {
        email: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error(
        "failed to assign admin role {role_id} to initial admin '{email}' (user {user_id}): {source}"
    )]
    AssignAdminRole {
        email: String,
        user_id: i32,
        role_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("failed to commit initial-admin transaction for '{email}': {source}")]
    CommitTransaction {
        email: String,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("failed to {operation} for initial admin '{email}': {source}")]
    InteractivePrompt {
        email: String,
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Configuration(#[from] InitialAdminConfigError),
}

#[derive(Debug, thiserror::Error)]
enum InitialAdminConfigError {
    #[error("TEMPS_ADMIN_EMAIL must be a valid email address")]
    InvalidEmail,
    #[error("TEMPS_ADMIN_EMAIL and TEMPS_ADMIN_PASSWORD_FILE must be configured together")]
    IncompleteCredentials,
    #[error("failed to read initial admin password file '{path}': {source}")]
    ReadPasswordFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("initial admin password in '{path}' does not meet complexity requirements: {reason}")]
    InvalidPassword { path: PathBuf, reason: String },
    #[error(
        "initial admin '{email}' is soft-deleted; restore it or choose a different TEMPS_ADMIN_EMAIL"
    )]
    DeletedUser { email: String },
    #[error("environment variable {name} is not valid Unicode: {source}")]
    InvalidEnvironment {
        name: &'static str,
        #[source]
        source: std::env::VarError,
    },
}

fn optional_environment_variable(
    name: &'static str,
) -> Result<Option<String>, InitialAdminConfigError> {
    optional_environment_variable_result(name, std::env::var(name))
}

fn optional_environment_variable_result(
    name: &'static str,
    result: Result<String, std::env::VarError>,
) -> Result<Option<String>, InitialAdminConfigError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(source @ std::env::VarError::NotUnicode(_)) => {
            Err(InitialAdminConfigError::InvalidEnvironment { name, source })
        }
    }
}

fn normalize_configured_admin_email(value: &str) -> Result<String, InitialAdminConfigError> {
    let email = value.trim().to_lowercase();
    if !temps_email::is_valid_email_syntax(&email) {
        return Err(InitialAdminConfigError::InvalidEmail);
    }

    Ok(email)
}

fn configured_initial_admin(
    email: Option<&str>,
    password_file: Option<&str>,
) -> Result<Option<(String, String)>, InitialAdminConfigError> {
    let (Some(email), Some(password_file)) = (email, password_file) else {
        return if email.is_none() && password_file.is_none() {
            Ok(None)
        } else {
            Err(InitialAdminConfigError::IncompleteCredentials)
        };
    };

    let email = normalize_configured_admin_email(email)?;
    let path = PathBuf::from(password_file);
    let password_file_contents = std::fs::read_to_string(&path).map_err(|source| {
        InitialAdminConfigError::ReadPasswordFile {
            path: path.clone(),
            source,
        }
    })?;
    let password = password_file_contents
        .trim_end_matches(['\r', '\n'])
        .to_string();
    temps_auth::validate_password_complexity(&password).map_err(|error| {
        InitialAdminConfigError::InvalidPassword {
            path,
            reason: error.to_string(),
        }
    })?;

    Ok(Some((email, password)))
}

fn ensure_existing_initial_admin_is_active(
    is_deleted: bool,
    email: &str,
) -> Result<(), InitialAdminConfigError> {
    if is_deleted {
        return Err(InitialAdminConfigError::DeletedUser {
            email: email.to_string(),
        });
    }
    Ok(())
}

/// `TEMPS_CLOUD_ENROLLMENT_CODE` -- a first-boot bootstrap *input*, not a
/// runtime setting.
///
/// This is not configuration in the sense CLAUDE.md's "no env vars for
/// configuration" rule forbids: nothing reads it to decide behaviour, and it
/// is never re-read for the life of the process. It is the same sanctioned
/// first-boot exception as `TEMPS_ADMIN_EMAIL`/`TEMPS_ADMIN_PASSWORD_FILE`
/// above -- a one-shot input consumed once, whose *result* is what gets
/// persisted: the Cloud link row (encrypted, via `CloudService::enroll`) and
/// a `CLOUD_LINK_CONNECTED` audit record, exactly as if an operator had
/// entered the code under Settings > Cloud. Like the admin bootstrap, it is
/// necessarily read after the database is up, because the thing it creates
/// lives in the database. The value itself is a short-lived, single-use
/// enrollment code minted for this instance's Temps Cloud tenant (see the
/// Cloud repo's ADR 0040), so an automated provisioning flow can link a
/// freshly booted instance with no human present to paste the code in.
///
/// Enrollment through this variable is best-effort and must never block or
/// fail server startup: it runs on its own task (see
/// [`bootstrap_cloud_enrollment_from_env`]), and an invalid, expired, or
/// already-consumed code, or an unreachable backend, degrades to a warning
/// log while the server starts normally. An instance that is already linked
/// treats the variable as a silent no-op instead of attempting to re-enroll,
/// so a leftover value from a previous boot (e.g. a `.env` a provisioning
/// tool reused) is harmless -- and that decision is made atomically with the
/// enrollment (`CloudService::enroll_link_if_unlinked`), so a link an
/// operator establishes while the code is in flight is never replaced.
const TEMPS_CLOUD_ENROLLMENT_CODE_VAR: &str = "TEMPS_CLOUD_ENROLLMENT_CODE";

/// Interprets a raw `std::env::var(TEMPS_CLOUD_ENROLLMENT_CODE_VAR)` result.
///
/// Split out (mirroring [`optional_environment_variable_result`] above) so a
/// test can drive every outcome -- absent, blank, non-unicode, present -- by
/// passing a synthetic `Result` directly, without mutating the real process
/// environment, which would race with other tests running in parallel in
/// this binary.
fn parse_cloud_enrollment_code_env(result: Result<String, std::env::VarError>) -> Option<String> {
    match result {
        Ok(code) if !code.trim().is_empty() => Some(code),
        Ok(_) | Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            warn!(
                "{TEMPS_CLOUD_ENROLLMENT_CODE_VAR} is set but is not valid UTF-8; skipping \
                 unattended Temps Cloud enrollment. Unset it, or set it to the enrollment \
                 code text, and restart to retry."
            );
            None
        }
    }
}

/// Upper bound on each network step of one unattended enrollment attempt
/// (redeeming the code, then fetching the managed-backup credential). The
/// Cloud client has its own per-request timeouts; this caps each step so a
/// wedged backend can never leave the task -- and its "enrollment in
/// progress" log state -- hanging around for the life of the process.
const CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long graceful shutdown waits for an in-flight unattended enrollment
/// before letting the runtime drop it. Generous relative to the local audit
/// write it exists to protect (see [`run_cloud_enrollment_bootstrap`]), tight
/// relative to the network timeout above: a shutdown must never be held
/// hostage by an unreachable Cloud backend.
const CLOUD_ENROLLMENT_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Kick off unattended Temps Cloud enrollment from
/// [`TEMPS_CLOUD_ENROLLMENT_CODE_VAR`], if it is set.
///
/// Called once during startup, after plugin services (including
/// `CloudService`) have finished registering and initializing -- and
/// deliberately **not awaited** there. The attempt runs on its own task so a
/// slow or unreachable Cloud backend can never delay the listeners binding
/// or `/readyz` turning healthy: enrollment is a side effect of startup, not
/// a precondition for it. Returns the task handle so the caller can join it
/// (bounded by [`CLOUD_ENROLLMENT_SHUTDOWN_GRACE`]) at graceful shutdown,
/// and so a test can join it deterministically. Nothing here can fail
/// startup: every failure mode is logged inside the task and swallowed, per
/// the ADR's requirement that this be best-effort.
fn bootstrap_cloud_enrollment_from_env(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
) -> Option<tokio::task::JoinHandle<()>> {
    let code = parse_cloud_enrollment_code_env(std::env::var(TEMPS_CLOUD_ENROLLMENT_CODE_VAR))?;

    let Some(cloud_service) = service_context.get_service::<CloudService>() else {
        warn!(
            "{TEMPS_CLOUD_ENROLLMENT_CODE_VAR} is set but this build has no Cloud plugin \
             registered; skipping unattended enrollment"
        );
        return None;
    };
    // `get_service`, not `require_service`: the audit logger is registered
    // by the audit plugin, and a build without one must still be able to
    // enroll -- the missing trail is reported loudly inside the task.
    let audit_logger = service_context.get_service::<dyn temps_core::AuditLogger>();

    Some(tokio::spawn(async move {
        let enroll_service = cloud_service.clone();
        let provision_service = cloud_service.clone();
        let console_access_service = cloud_service;
        let link_audit = audit_logger.clone();
        let backup_audit = audit_logger;
        run_cloud_enrollment_bootstrap(
            &code,
            move |code| async move { enroll_service.enroll_link_if_unlinked(&code).await },
            move || async move {
                match &link_audit {
                    Some(audit_logger) => {
                        temps_cloud::record_link_connected_audit(
                            audit_logger.as_ref(),
                            CloudEnrollmentActor::UnattendedBootstrap,
                        )
                        .await;
                    }
                    None => error!(
                        "Unattended Temps Cloud enrollment succeeded but no audit logger is \
                         registered; the CLOUD_LINK_CONNECTED audit record was not written"
                    ),
                }
                // ADR-045 §5: the unattended first-boot bootstrap is the one
                // enrollment path that defaults console access *on* -- set
                // it here, once, immediately after the link's own audit row,
                // and audit the default itself. A failure here is logged
                // and never turns a successful enrollment into a failure;
                // console access simply stays off until an operator flips
                // it from Settings > Temps Cloud.
                match console_access_service
                    .enable_console_access_for_unattended_bootstrap()
                    .await
                {
                    Ok(()) => {
                        if let Some(audit_logger) = &link_audit {
                            temps_cloud::record_console_access_default_enabled_audit(
                                audit_logger.as_ref(),
                                CloudEnrollmentActor::UnattendedBootstrap,
                            )
                            .await;
                        }
                    }
                    Err(error) => warn!(
                        %error,
                        "Unattended Temps Cloud enrollment succeeded but console access could \
                         not be enabled by default; enable it from Settings > Temps Cloud"
                    ),
                }
            },
            move || async move {
                provision_service
                    .provision_managed_backups_after_enrollment()
                    .await
            },
            move |backup_outcome| async move {
                if let Some(audit_logger) = &backup_audit {
                    temps_cloud::record_backup_outcome_audit(
                        audit_logger.as_ref(),
                        CloudEnrollmentActor::UnattendedBootstrap,
                        &backup_outcome,
                    )
                    .await;
                }
            },
        )
        .await;
    }))
}

/// Await an in-flight unattended enrollment at graceful shutdown, for at
/// most [`CLOUD_ENROLLMENT_SHUTDOWN_GRACE`]. Without this the detached task
/// would simply be dropped with the runtime, which could tear it down in the
/// narrow window between persisting the Cloud credential and writing its
/// audit row.
async fn join_cloud_enrollment_bootstrap(handle: Option<tokio::task::JoinHandle<()>>) {
    let Some(handle) = handle else {
        return;
    };
    if handle.is_finished() {
        return;
    }
    info!("Waiting for an in-flight unattended Temps Cloud enrollment to finish...");
    match tokio::time::timeout(CLOUD_ENROLLMENT_SHUTDOWN_GRACE, handle).await {
        Ok(Ok(())) => {}
        Ok(Err(join_error)) => warn!(
            %join_error,
            "Unattended Temps Cloud enrollment task ended abnormally during shutdown"
        ),
        Err(_elapsed) => warn!(
            grace_secs = CLOUD_ENROLLMENT_SHUTDOWN_GRACE.as_secs(),
            "Unattended Temps Cloud enrollment was still waiting on the Cloud backend at \
             shutdown; abandoning it. If the instance shows as linked after restart but \
             has no CLOUD_LINK_CONNECTED audit entry, this is why."
        ),
    }
}

/// The testable core of [`bootstrap_cloud_enrollment_from_env`].
///
/// Takes every side-effecting step as a parameter (rather than a concrete
/// `CloudService`) so a unit test can drive every enrollment outcome --
/// established, already linked, lost the race, failed, timed out -- and
/// check that the audit hooks fire exactly when state was persisted, without
/// constructing a real `CloudService`, which needs a live database connection
/// and managed-backend configuration this module does not otherwise depend on.
///
/// Whether the instance is already linked is *not* decided here. It is
/// decided by `CloudService::enroll_link_if_unlinked`, atomically with the
/// enrollment itself, because an operator can complete `POST /cloud/enroll`
/// at any point while this task is waiting on the Cloud backend, and a
/// snapshot taken out here would let a delayed environment-code enrollment
/// replace the tenant that operator chose.
///
/// Ordering is the point. Enrollment persists state twice, with a network
/// round-trip in between (`CloudService::enroll_link_if_unlinked`, then
/// `CloudService::provision_managed_backups_after_enrollment`), so each
/// network step gets its own [`CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT`] and the
/// matching audit hook runs *immediately* after the step that persisted
/// something, unbounded. A timeout on the backup step therefore never
/// affects the link's audit row -- the link is already recorded -- and the
/// log it produces says so, rather than claiming no link exists.
async fn run_cloud_enrollment_bootstrap<
    E,
    EnrollFut,
    L,
    LinkAuditFut,
    P,
    ProvisionFut,
    B,
    BackupAuditFut,
>(
    code: &str,
    enroll_link: E,
    record_link_audit: L,
    provision_backups: P,
    record_backup_audit: B,
) where
    E: FnOnce(String) -> EnrollFut,
    EnrollFut: std::future::Future<Output = Result<FirstLinkEnrollment, CloudServiceError>>,
    L: FnOnce() -> LinkAuditFut,
    LinkAuditFut: std::future::Future<Output = ()>,
    P: FnOnce() -> ProvisionFut,
    ProvisionFut: std::future::Future<Output = ManagedBackupOutcome>,
    B: FnOnce(ManagedBackupOutcome) -> BackupAuditFut,
    BackupAuditFut: std::future::Future<Output = ()>,
{
    let kind = match tokio::time::timeout(
        CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT,
        enroll_link(code.to_string()),
    )
    .await
    {
        Ok(Ok(FirstLinkEnrollment::Established(kind))) => kind,
        Ok(Ok(FirstLinkEnrollment::AlreadyLinked)) => {
            debug!(
                "{TEMPS_CLOUD_ENROLLMENT_CODE_VAR} is set but this instance is already linked \
                 to Temps Cloud; ignoring it rather than re-enrolling"
            );
            return;
        }
        Ok(Ok(FirstLinkEnrollment::LostRaceToConcurrentEnrollment)) => {
            // Nothing was persisted by this task, so there is nothing to
            // audit here -- the enrollment that won recorded its own trail.
            warn!(
                "Unattended Temps Cloud enrollment via {TEMPS_CLOUD_ENROLLMENT_CODE_VAR} was \
                 abandoned: another enrollment completed while the code was being redeemed, \
                 and that link stands. The code has been consumed on the Cloud side, so if the \
                 Cloud console shows this instance as connected to a tenant it is not actually \
                 linked to, disconnect it there."
            );
            return;
        }
        Err(_elapsed) => {
            warn!(
                timeout_secs = CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT.as_secs(),
                "Unattended Temps Cloud enrollment via {TEMPS_CLOUD_ENROLLMENT_CODE_VAR} \
                 timed out waiting for the Cloud backend; continuing without a Cloud link. \
                 This is non-fatal -- mint a fresh code and restart to retry, or connect \
                 from Settings > Cloud once the backend is reachable."
            );
            return;
        }
        Ok(Err(error)) => {
            warn!(
                %error,
                "Unattended Temps Cloud enrollment via {TEMPS_CLOUD_ENROLLMENT_CODE_VAR} \
                 failed; continuing startup without a Cloud link. This is non-fatal -- mint a \
                 fresh code and restart to retry, or connect from Settings > Cloud once the \
                 server is up."
            );
            return;
        }
    };
    // The credential is on disk from here on: record it before anything
    // else can go wrong.
    record_link_audit().await;
    info!(
        enrollment = kind.as_str(),
        "Unattended Temps Cloud enrollment via {TEMPS_CLOUD_ENROLLMENT_CODE_VAR} succeeded"
    );

    match tokio::time::timeout(CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT, provision_backups()).await {
        Ok(backup_outcome) => record_backup_audit(backup_outcome).await,
        Err(_elapsed) => warn!(
            timeout_secs = CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT.as_secs(),
            "The Cloud link is established, but fetching the managed backup credential timed \
             out; managed backups will be set up on the next credential rotation, or from \
             Settings > Cloud."
        ),
    }
}

fn prompt_for_admin_email() -> anyhow::Result<Option<String>> {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
    );
    println!(
        "{}",
        "           🚀 Welcome to Temps!".bright_white().bold()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
    );
    println!();
    println!(
        "{}",
        "No users found. Let's set up your admin account!".bright_yellow()
    );
    println!();
    println!("{}", "This email will be used for:".bright_white());
    println!("  {} Admin account access", "•".bright_cyan());
    println!("  {} Let's Encrypt SSL certificates", "•".bright_cyan());
    println!("  {} Important system notifications", "•".bright_cyan());
    println!();

    print!(
        "{} ",
        "Please enter your email address:".bright_white().bold()
    );
    io::stdout().flush()?;

    let mut email = String::new();
    io::stdin().read_line(&mut email)?;
    let email = email.trim().to_lowercase();

    if !temps_email::is_valid_email_syntax(&email) {
        println!();
        println!(
            "{}",
            "⚠️  Invalid email address. Please provide a valid email.".bright_red()
        );
        return Ok(None);
    }

    println!();
    println!(
        "{} {}",
        "✅ Email configured:".bright_green(),
        email.bright_white()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
    );
    println!();

    Ok(Some(email))
}

fn create_openapi(plugin_manager: &PluginManager) -> anyhow::Result<utoipa::openapi::OpenApi> {
    let mut api_doc = plugin_manager
        .get_unified_openapi()
        .map_err(|e| anyhow::anyhow!("Failed to build unified OpenAPI schema: {}", e))?;

    // Merge node registration endpoints (not part of the plugin system)
    let nodes_doc = <temps_deployments::handlers::nodes::NodesApiDoc as utoipa::OpenApi>::openapi();
    api_doc.merge(nodes_doc);

    // Merge admin-gate management endpoints (also not part of the plugin system)
    let gate_doc = <super::admin_gate_handler::AdminGateApiDoc as utoipa::OpenApi>::openapi();
    api_doc.merge(gate_doc);

    Ok(api_doc)
}

/// Axum middleware that rejects unauthenticated requests to the Swagger UI
/// and OpenAPI JSON endpoint. The auth middleware stack must have already run
/// (i.e. be an outer layer) so the `AuthContext` extension is present for
/// authenticated callers. Anonymous callers — with no valid session cookie or
/// Bearer token — receive a 401 with a WWW-Authenticate hint.
async fn require_auth_for_docs(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header::WWW_AUTHENTICATE;
    if req.extensions().get::<temps_auth::AuthContext>().is_some() {
        next.run(req).await
    } else {
        Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(WWW_AUTHENTICATE, "Bearer realm=\"temps\"")
            .header(header::CONTENT_TYPE, "application/problem+json")
            .body(Body::from(
                r#"{"type":"about:blank","title":"Unauthorized","status":401,"detail":"Authentication required to access the API documentation."}"#,
            ))
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }
}

fn create_swagger_router(plugin_manager: &PluginManager) -> anyhow::Result<Router> {
    let api_doc = create_openapi(plugin_manager)?;
    // Build the raw Swagger router, then add the auth-guard as an inner layer
    // (applied after the outer auth middleware has already run and injected the
    // `AuthContext` extension). Axum applies `.layer()` calls in reverse order:
    // the *last* `.layer()` wraps outermost (runs first).  Because
    // `apply_middleware_to_router` is called after we add the guard here,
    // the plugin auth middleware is the outermost shell → runs first →
    // populates `AuthContext` → then the guard reads it.
    let swagger =
        Router::new().merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api_doc));
    // Add the auth-guard as the innermost layer (runs after auth middleware).
    let swagger_guarded = swagger.layer(axum::middleware::from_fn(require_auth_for_docs));
    // Wrap with the full plugin middleware stack (auth context injection, etc.).
    Ok(plugin_manager.apply_middleware_to_router(swagger_guarded, plugin_manager.get_middleware()))
}

/// Static file handler for embedded website
async fn serve_static_file(req: Request) -> Response {
    serve_static_from(website(), req)
}

/// Same, but always the ORIGINAL console bundle (for the platform-console
/// listener when the root bundle has been overridden by an embedding binary).
async fn serve_original_console(req: Request) -> Response {
    serve_static_from(&WEBSITE, req)
}

fn serve_static_from(site: &'static Dir<'static>, req: Request) -> Response {
    let raw_path = req.uri().path();

    // Never serve the SPA for /api/ paths — those should 404 if unmatched
    // by any API router (including external plugin proxies).
    if raw_path.starts_with("/api/") {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"status":404,"title":"Not Found","detail":"No API route matched"}"#,
            ))
            .unwrap();
    }

    // Remove leading slash
    let path = raw_path.strip_prefix('/').unwrap_or(raw_path);

    // Default to index.html for directory requests or root
    let path = if path.is_empty() || path.ends_with('/') {
        "index.html"
    } else {
        path
    };

    debug!("Attempting to serve static file: {}", path);

    match site.get_file(path) {
        Some(file) => {
            let mime_type = mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string();

            // Hashed assets (JS/CSS bundles from Rsbuild) get aggressive caching.
            // index.html and other non-hashed files must revalidate every time so
            // deploying a new Temps version immediately picks up new bundle references.
            let cache_control = if path == "index.html" || path == "/" {
                "no-cache, no-store, must-revalidate"
            } else if path.starts_with("static/") || path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=0, must-revalidate"
            };

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime_type)
                .header(header::CACHE_CONTROL, cache_control)
                .body(Body::from(file.contents()))
                .unwrap()
        }
        None => {
            // If file not found, try serving index.html (for SPA routing)
            if let Some(index) = site.get_file("index.html") {
                debug!("File not found, serving index.html for SPA routing");
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(Body::from(index.contents()))
                    .unwrap()
            } else {
                debug!("File not found and no index.html available: {}", path);
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("404 Not Found"))
                    .unwrap()
            }
        }
    }
}

/// Download GeoLite2-City.mmdb to `dest` when it is missing.
///
/// Delegates to `temps_geo::refresh`, which owns every download path: the
/// source selection (MaxMind with a license key, the repository copy without
/// one), validation that the bytes really are a parseable database, and the
/// `.tmp`-then-rename write. The scheduled refresh job calls the same code, so
/// startup recovery and periodic refresh can no longer drift apart.
/// `license_key` is the plaintext key the caller decrypted from the settings
/// row, or `None` to use the repository copy. It is never logged here.
async fn download_geolite2_database_on_startup(
    dest: &Path,
    license_key: Option<&str>,
) -> anyhow::Result<()> {
    let http = temps_geo::refresh::build_http_client()
        .map_err(|e| anyhow::anyhow!("Failed to build the GeoLite2 download client: {}", e))?;

    info!(
        "Downloading GeoLite2-City.mmdb to {} (MaxMind license key configured: {})",
        dest.display(),
        license_key.is_some()
    );

    temps_geo::refresh::ensure_mmdb_present(dest, license_key, &http)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "{}",
                temps_geo::redact_license_key(e.to_string(), license_key)
            )
        })?;

    Ok(())
}

/// Read the MaxMind license key configured in the settings row, decrypted.
///
/// A key that cannot be read or decrypted is reported as "no key" rather than
/// as a startup failure: the download then falls back to the repository copy,
/// which is strictly better than refusing to boot over an optional credential.
/// The plaintext is returned to the single caller below and never logged.
async fn configured_maxmind_license_key(
    db: &Arc<DbConnection>,
    encryption_service: &Arc<EncryptionService>,
) -> Option<String> {
    let settings = match temps_entities::settings::Entity::find_by_id(1)
        .one(db.as_ref())
        .await
    {
        Ok(Some(row)) => temps_core::AppSettings::from_json(row.data),
        Ok(None) => return None,
        Err(e) => {
            warn!(
                "Could not read the settings row for the MaxMind license key; falling back to \
                 the bundled GeoLite2 source: {}",
                e
            );
            return None;
        }
    };

    match settings
        .geo
        .decrypt_license_key(encryption_service.as_ref())
    {
        Ok(key) => key,
        Err(e) => {
            warn!(
                "Could not decrypt the stored MaxMind license key; falling back to the bundled \
                 GeoLite2 source: {}",
                e
            );
            None
        }
    }
}

/// Validate GeoLite2-City database exists in multiple locations.
/// Checks current directory and data directory; if neither has the file,
/// downloads it to `default_db_path`. Errors only after the download attempt
/// fails.
async fn validate_geolite2_database(
    default_db_path: &Path,
    license_key: Option<&str>,
) -> anyhow::Result<()> {
    // Check multiple locations in order of preference
    let search_paths = vec![
        // 1. Current working directory (most convenient for local development)
        PathBuf::from("./GeoLite2-City.mmdb"),
        // 2. Data directory (from config)
        default_db_path.to_path_buf(),
    ];

    // Try to find the database in any of the search paths
    for path in &search_paths {
        if path.exists() {
            debug!("✓ GeoLite2 database found at: {}", path.display());
            return Ok(());
        }
    }

    // Not found anywhere — attempt automatic download to the data directory,
    // matching the behaviour of `temps setup`.
    info!(
        "GeoLite2 database not found in {} or {}; attempting download",
        search_paths[0].display(),
        search_paths[1].display()
    );
    match download_geolite2_database_on_startup(default_db_path, license_key).await {
        Ok(()) => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "❌ GeoLite2-City.mmdb not found and automatic download failed\n\n\
            The MaxMind GeoLite2 database is required for geolocation features.\n\n\
            📍 Checked locations (in order):\n\
            1. {}\n\
            2. {}\n\n\
            ⬇️  Download attempt error: {}\n\n\
            📥 Manual setup (takes 2 minutes):\n\
            1. Visit: https://www.maxmind.com/en/geolite2/geolite2-free-data-sources\n\
            2. Create free MaxMind account (if needed)\n\
            3. Download 'GeoLite2-City' (GZIP format: .tar.gz)\n\
            4. Extract the archive:\n\
               tar xzf GeoLite2-City_*.tar.gz\n\n\
            5. Copy the database file to any location above:\n\
               # Option A: Current directory (recommended for local development)\n\
               cp GeoLite2-City_*/GeoLite2-City.mmdb .\n\n\
               # Option B: Data directory\n\
               cp GeoLite2-City_*/GeoLite2-City.mmdb {}\n\n\
            6. Start the server again\n\n\
            🔑 Automatic downloads and refreshes:\n\
            Add a MaxMind license key (free MaxMind account) under\n\
            Settings → Metrics Monitoring → Geolocation database, and Temps\n\
            downloads GeoLite2-City itself and re-checks it on the refresh\n\
            interval configured there (default every 24 hours).\n\n\
            🐳 For Docker users:\n\
            See Dockerfile in the repository for embedding the database",
            search_paths[0].display(),
            search_paths[1].display(),
            e,
            search_paths[1].display()
        )),
    }
}

/// Parameters for starting the console API server.
///
/// Groups the dependencies needed by [`start_console_api`] to keep the
/// function signature under clippy's argument limit.
pub struct ConsoleApiParams {
    pub db: Arc<DbConnection>,
    pub config: Arc<ServerConfig>,
    pub cookie_crypto: Arc<CookieCrypto>,
    pub encryption_service: Arc<EncryptionService>,
    pub route_table: Arc<temps_proxy::CachedPeerTable>,
    pub queue: Arc<dyn temps_core::JobQueue>,
    /// Fires once, right after plugin two-phase init completes (see the call
    /// site in `start_console_api`) — earlier and narrower than `/readyz`,
    /// which additionally waits for routers/middleware/listeners. The caller
    /// (`commands/serve/mod.rs`, single-binary mode) blocks the proxy's
    /// startup on this specifically to know whether the `project_ip_gate`
    /// slot has been claimed before serving any proxied traffic.
    pub ready_signal: Option<tokio::sync::oneshot::Sender<()>>,
    pub additional_templates: Vec<std::path::PathBuf>,
    pub on_demand_waker: Option<Arc<dyn temps_core::OnDemandWaker>>,
    /// Additional plugins registered by an external entrypoint (e.g. the
    /// EE binary). Registered immediately before `initialize_plugins`, so
    /// they observe every OSS service in the registry and can wrap or
    /// extend them. OSS callers pass an empty Vec.
    pub extra_plugins: Vec<Box<dyn TempsPlugin>>,
    /// Pre-built admin-gate service (when the caller wired the gate up
    /// outside the console). When `None`, the console builds its own.
    pub admin_gate_service: Option<super::admin_gate_service::AdminGateService>,
    /// Pre-built admin-gate handle. When `None`, the console derives one
    /// from the freshly-constructed service above.
    pub admin_gate_handle: Option<temps_core::admin_gate::AdminGateHandle>,
    /// Shared retention-resolver slot (see `temps_core::retention`). Owned by
    /// the caller (`commands/serve/mod.rs`) so the SAME instance can also be
    /// handed to `start_proxy_server` — the live Pingora proxy builds its own
    /// isolated plugin context (`temps-proxy/src/server.rs::setup_proxy_plugins`,
    /// only `ConfigPlugin`+`GeoPlugin`) and can never see a resolver registered
    /// here via the console's plugin manager otherwise. Pre-registered into the
    /// service registry below (before any plugin's `register_services` runs) so
    /// `ProxyPlugin` uses this exact object rather than creating its own.
    ///
    /// **Security guardrail — shared-slot pattern:**
    /// This is the only object that is deliberately pre-registered in the
    /// console's service registry AND passed directly to `start_proxy_server` as
    /// a constructor argument. That cross-context sharing is necessary here
    /// because the Pingora proxy runs in a wholly separate plugin context and
    /// cannot reach anything registered in the console's registry.
    ///
    /// This pattern MUST NOT be used for any object that affects the proxy's
    /// security decisions (authentication, TLS/cert issuance, IP blocklists,
    /// rate limiting, or routing). It is acceptable for `RetentionResolverSlot`
    /// exclusively because its sole effect is a per-row metadata value
    /// (`retention_days`) with no bearing on request routing, authorization, or
    /// connection handling. Any future object shared this same way requires an
    /// explicit security review before being added here.
    pub retention_resolver_slot: Arc<temps_core::RetentionResolverSlot>,
    /// Shared per-project/environment IP-restriction gate. Uses the exact
    /// same cross-context shared-slot mechanism as `retention_resolver_slot`
    /// immediately above — for the same structural reason: the Pingora
    /// proxy bootstraps in a wholly separate plugin context and has no
    /// other way to see something a plugin registered into the console's
    /// registry.
    ///
    /// **This is explicitly the category of object the guardrail above says
    /// requires review, not an exception to it.** `ProjectIpGate` decides
    /// which requests reach a deployed project/environment at all — it is a
    /// routing/authorization decision, not inert metadata. It is wired this
    /// way pending a security review, not because the guardrail was judged
    /// not to apply. Do not treat this as a second precedent for adding
    /// further objects to the shared-slot pattern without their own review.
    pub project_ip_gate_slot: Arc<temps_core::ProjectIpGateSlot>,
    pub request_policy_gate_slot: Arc<temps_core::RequestPolicyGateSlot>,
    /// Shared "a newer release exists" slot. Owned by the caller
    /// (`commands/serve/mod.rs`), which spawns the background update
    /// notifier that writes into it; registered into the service registry
    /// below so the settings API can serve it to the web console's upgrade
    /// banner (`GET /settings/update-status`). Advisory read-only metadata —
    /// it never influences routing, auth, or connection handling.
    pub update_status: Arc<temps_core::UpdateStatusSlot>,
    /// Applies a release update on request from the settings API and exits so
    /// the supervisor restarts temps on the new binary. Owned by the caller
    /// (`commands/serve/mod.rs`) so the journal of a previous attempt is
    /// resolved exactly once per process; registered below for ConfigPlugin's
    /// `GET/POST /settings/update`.
    pub self_updater: Arc<crate::commands::serve::self_update::BinarySelfUpdater>,
    /// Startup-resolved state of Traefik label discovery. Built by
    /// `commands/serve/mod.rs` (which decides whether the watcher actually
    /// runs) and registered into the service registry below so
    /// `GET /traefik-discovery/status` can report the truth — including
    /// `configured: false` plus the reason and the env vars that would enable
    /// it, rather than the endpoint disappearing on a default install.
    ///
    /// Read-only status metadata: it never influences routing, auth, or
    /// connection handling. The watcher's writes reach the route table through
    /// the `route_table_changes` NOTIFY path, not through this handle.
    pub traefik_discovery: Arc<temps_deployer::traefik_discovery::TraefikDiscoveryHandle>,
    /// Authenticated external-plugin registry configuration resolved from the
    /// paired `temps serve` bootstrap options.
    pub external_plugin_registry: temps_external_plugins::catalog::RegistryConfig,
    /// Whether this process runs workloads itself. Decides which plugins are
    /// constructed at all — see `register_local_workload_plugins` — and is
    /// published to clients through `GET /api/platform/features`.
    pub profile: super::ServeProfile,
}

/// How long the `control-plane` profile waits for a Docker ping before
/// deciding the daemon is unavailable. Short on purpose: this runs on the
/// startup path, and "no daemon" is an expected, supported answer here.
const DOCKER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The operator-facing message for "this profile needs Docker and it isn't
/// there". Shared by both failure points so the remediation steps can't drift
/// apart.
fn docker_unavailable_error(reason: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "❌ Docker dependency check FAILED\n\n\
        The system requires Docker to be running and accessible.\n\n\
        Error details: {}\n\n\
        Solutions:\n\
        1. Ensure Docker daemon is running\n\
           - macOS: Check Docker Desktop application\n\
           - Linux: Run 'sudo systemctl start docker'\n\n\
        2. Verify Docker socket permissions\n\
           - Linux: Run 'sudo usermod -aG docker $USER'\n\n\
        3. Check Docker environment variables\n\
           - DOCKER_HOST may need to be set\n\n\
        4. Run this control plane without local workloads\n\
           - `temps serve --profile control-plane` needs no Docker daemon; \
             applications then run on worker nodes joined with `temps join`\n\n\
        Deployment features will not be available until Docker is accessible.",
        reason
    )
}

/// Storage backend selection for the log aggregator.
#[derive(Debug, thiserror::Error)]
pub enum LogStorageConfigError {
    #[error(
        "TEMPS_LOG_STORAGE_BACKEND is set to 's3', but {variable} is not set. Set it (and the \
         other TEMPS_LOG_S3_* variables), or unset TEMPS_LOG_STORAGE_BACKEND to store aggregated \
         logs on local disk"
    )]
    MissingS3Variable { variable: &'static str },
}

/// Resolve the log-aggregator storage backend.
///
/// Previously three `std::env::var(..).expect(..)` calls on the startup path:
/// an operator who set `TEMPS_LOG_STORAGE_BACKEND=s3` and forgot one variable
/// got a panic with a bare message and no indication that the other two would
/// have failed as well. Returns a typed error the caller renders instead.
fn log_aggregator_storage_config(
    data_dir: &std::path::Path,
) -> Result<StorageConfig, LogStorageConfigError> {
    fn required(variable: &'static str) -> Result<String, LogStorageConfigError> {
        std::env::var(variable)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(LogStorageConfigError::MissingS3Variable { variable })
    }

    let backend =
        std::env::var("TEMPS_LOG_STORAGE_BACKEND").unwrap_or_else(|_| "filesystem".into());
    if backend != "s3" {
        return Ok(StorageConfig::Filesystem {
            base_path: data_dir.join("log-aggregator"),
        });
    }

    Ok(StorageConfig::S3 {
        bucket: required("TEMPS_LOG_S3_BUCKET")?,
        region: std::env::var("TEMPS_LOG_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
        endpoint: std::env::var("TEMPS_LOG_S3_ENDPOINT").ok(),
        access_key_id: required("TEMPS_LOG_S3_ACCESS_KEY_ID")?,
        secret_access_key: required("TEMPS_LOG_S3_SECRET_ACCESS_KEY")?,
        prefix: Some(std::env::var("TEMPS_LOG_S3_PREFIX").unwrap_or_else(|_| "logs/".to_string())),
        force_path_style: std::env::var("TEMPS_LOG_S3_FORCE_PATH_STYLE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false),
    })
}

/// Build a ClickHouse-backed metrics store from the server config, or `None`
/// when ClickHouse is not configured.
///
/// Returns `Some(store)` only when all four `TEMPS_CLICKHOUSE_*` vars are set
/// (`config.is_clickhouse_enabled()`). When the monitoring store is set to
/// ClickHouse but the env vars are absent, this logs a warning and returns
/// `None` so the caller falls back to TimescaleDB (fail-open to the default
/// path, never silently losing metrics). Construction does no I/O; migrations
/// are spawned separately by the caller.
fn build_ch_metrics_store(config: &ServerConfig) -> Option<Arc<dyn temps_metrics::MetricsStore>> {
    use temps_metrics::{ClickHouseMetricsConfig, ClickhouseMetricsStore, MetricsStore};

    if !config.is_clickhouse_enabled() {
        tracing::warn!(
            "Monitoring store is set to ClickHouse but TEMPS_CLICKHOUSE_* env vars are not \
             fully configured; falling back to TimescaleDB for resource metrics"
        );
        return None;
    }

    // is_clickhouse_enabled() guarantees all four are Some.
    let cfg = ClickHouseMetricsConfig::new(
        config.clickhouse_url.clone().unwrap_or_default(),
        config.clickhouse_database.clone().unwrap_or_default(),
        config.clickhouse_user.clone().unwrap_or_default(),
        config.clickhouse_password.clone().unwrap_or_default(),
    );
    let store = Arc::new(ClickhouseMetricsStore::new(cfg));

    // Run migrations in the background so startup is not blocked. If they fail,
    // the first write/read surfaces the error per-call. Guard on the runtime
    // handle: this is called from an async path today, but a bare tokio::spawn
    // panics if ever invoked from a sync context (no reactor) — fall back to a
    // short-lived current-thread runtime in that case.
    let client = store.client().clone();
    let database = config.clickhouse_database.clone().unwrap_or_default();
    let run_migrations = async move {
        match temps_metrics::clickhouse_migrations::apply_migrations(&client, &database).await {
            Ok(report) => debug!(
                applied = ?report.applied,
                skipped = report.skipped.len(),
                "ClickHouse resource-metrics migrations applied"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "ClickHouse resource-metrics migrations failed; \
                 metric writes/queries will surface the error per-call"
            ),
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(run_migrations);
        }
        Err(_) => match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(run_migrations),
            Err(e) => tracing::warn!(
                error = %e,
                "Could not build a runtime to apply ClickHouse resource-metrics \
                 migrations; they will be attempted on first read/write"
            ),
        },
    }

    Some(store as Arc<dyn MetricsStore>)
}

/// Build the console health/readiness router.
///
/// Two unauthenticated probes, mounted at the document root so a supervisor
/// (systemd, an external load balancer, or `temps upgrade`'s post-restart
/// health gate) can poll them directly:
///
/// - `GET /healthz` — **liveness**: always `200 OK` as long as the process is
///   serving HTTP. It does not assert that plugins finished initializing, so a
///   supervisor can tell "process is up" from "process is wedged" without
///   restarting a console that is merely mid-warmup.
/// - `GET /readyz` — **readiness**: `200 OK` only once routers, middleware,
///   the admin gate, and the listener(s) are all built and about to serve —
///   the shared `ready` flag flips immediately before `axum::serve`, later
///   than plugin init alone. Returns `503 Service Unavailable` while warming
///   up. This is the gate the split-topology upgrade flow polls before
///   declaring a console upgrade successful — binding the port is NOT
///   sufficient, because the router would otherwise answer 200 while every
///   real route still 500s during warmup.
///
/// The flag lives in an `Arc<AtomicBool>` shared with the serve loop rather
/// than reading a oneshot, so the probe stays truthful for the entire process
/// lifetime. It is deliberately later and separate from the `ready_signal`
/// oneshot `commands/serve/mod.rs` waits on to gate proxy startup — that one
/// fires as soon as plugin two-phase init completes (see its call site),
/// which is the earliest point the `project_ip_gate` slot's fate is settled,
/// well before routers/listeners are ready.
fn health_router(ready: Arc<std::sync::atomic::AtomicBool>) -> Router {
    use axum::routing::get;

    let readyz = {
        let ready = ready.clone();
        move || async move {
            if ready.load(std::sync::atomic::Ordering::Relaxed) {
                (StatusCode::OK, "ready")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "initializing")
            }
        }
    };

    Router::new()
        .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
        .route("/readyz", get(readyz))
}

/// ALLOWLIST for the AI `call_api` read tool — opt-in / secure by default.
/// The model may call ONLY these read-only GET operations; every other
/// endpoint (including any newly added one) is invisible to it. This is
/// deliberately an allowlist, not a denylist: GET endpoints across the
/// platform return decrypted secrets (service params → DB passwords,
/// env-var reveals, notification configs → Slack/SMTP/Cloudflare creds, S3
/// credentials, …), and a hand-maintained denylist can't be kept exhaustive.
/// Curated for the SRE/debugging use case: observability, runtime/deploy
/// status, errors, and MASKED metadata. Adding an entry is a security
/// decision — verify the operation's response contains NO decrypted
/// secret/token/key.
///
/// Analytics `custom_data`/`event_data` (freeform JSON on visitors/events)
/// is an ACCEPTED risk, not an oversight: it's developer-controlled data the
/// project operator already sends themselves, gated by the same
/// AnalyticsRead + project scope as every other analytics tool below — not
/// a platform secret. Raw HTTP request/response headers (session logs) and
/// session-replay DOM/keystroke capture are excluded on purpose; those can
/// carry Authorization/Cookie values or typed passwords and are a different
/// risk class entirely.
///
/// Extracted into its own function (rather than an inline literal at the
/// call site) so tests can assert every entry resolves to a real OpenAPI
/// operation_id without duplicating the list — see `ai_tool_allowlist_tests`
/// at the bottom of this file.
fn ai_read_allowlist() -> Vec<String> {
    [
        // ── OpenTelemetry: metrics / traces / logs / health ──
        "query_metrics",
        "list_metric_names",
        "list_metric_label_keys",
        "list_metric_label_values",
        "query_traces",
        "query_trace_summaries",
        "get_trace",
        "query_genai_traces",
        "get_genai_trace",
        "query_logs",
        "list_insights",
        "get_health",
        "get_quota",
        "get_pipeline_stats",
        // ── Container runtime: logs + metrics (no secrets) ──
        "get_container_metrics",
        "get_container_logs",
        "get_container_logs_by_id",
        "get_container_info",
        "get_container_detail",
        "list_containers",
        // ── Projects: current-user-filtered metadata ──
        // The handler enforces ProjectsRead and derives hidden project ids
        // from the AuthContext forwarded by the current private chat turn.
        "get_projects",
        // ── Deployments: status / jobs / history ──
        "get_deployment",
        "get_last_deployment",
        "get_project_deployments",
        // Manual-deploy discovery: registered external images the
        // AI can deploy by id/ref (metadata only — no registry
        // credentials). Static bundles are frontend-only, so their
        // read ops are intentionally excluded here.
        "list_external_images",
        "get_external_image",
        "get_deployment_jobs",
        "get_deployment_operations",
        "get_deployment_operation_status",
        "get_activity_graph",
        "get_deployment_job_logs",
        "list_deployment_container_logs",
        "get_deployment_container_log_content",
        // ── Environments: metadata + MASKED env-var lists only ──
        // (the *_value reveal endpoints are intentionally excluded)
        "get_environments",
        "get_environment",
        "get_environment_domains",
        "get_environment_crons",
        "get_cron_by_id",
        "get_cron_executions",
        "get_environment_variables",
        "get_resolved_environment_variables",
        // ── Error tracking ──
        "get_error_dashboard_stats",
        "get_error_event",
        "get_error_group",
        "get_error_stats",
        "get_error_time_series",
        "has_error_groups",
        "list_error_events",
        "list_error_groups",
        "list_alert_rules",
        "get_alert_rule",
        // ── Metric alert rules (OTel) — the rules themselves, so the AI can
        //    see what is already alerted on before proposing anything new.
        //    Without these it proposes duplicates of rules that exist.
        "list_alerts",
        "get_alert",
        // Notification provider responses use decrypt_provider_config(),
        // which masks secret fields before serialization. That lets the AI
        // discover and manage providers without receiving reusable secrets.
        "list_notification_providers",
        "get_notification_provider",
        "list_notification_routes",
        "get_notification_route",
        // ── Service inventory / status / health / types (NOT params/env) ──
        // `list_services` is filtered by the current user's
        // ExternalServicesRead permission, and deployment tokens are rejected
        // by the handler. Without it, the AI can see only services already
        // linked to the current project and cannot discover a newly created
        // database in order to link or inspect it.
        "list_services",
        "get_service_health_status",
        "list_service_health_statuses",
        "get_service_stats",
        "get_service_runtime",
        "list_project_services",
        "list_service_projects",
        "get_service_types",
        "get_service_type_parameters",
        "get_cluster_health",
        "get_cluster_member",
        "getPostgresWalHealth",
        "ExternalServiceMetricsGetLatest",
        "ExternalServiceMetricsGetRange",
        "ExternalServiceMetricsStatus",
        "ExternalServiceMetricsByDatabase",
        "ExternalServiceMetricsGetAlertRules",
        "DeploymentMetricsGetRange",
        "DeploymentMetricsGetLatest",
        "NodeMetricsGetRange",
        "NodeMetricsGetLatest",
        // ── Domains: metadata (no challenge tokens) ──
        "list_domains",
        "get_domain",
        "get_domain_by_name",
        "get_domain_dns_records",
        "list_custom_domains_for_project",
        "get_custom_domain",
        "check_domain_status",
        "list_managed_domains",
        "get_on_demand_cert_status",
        // ── Platform / monitor status ──
        "get_status_overview",
        "get_disk_status",
        "get_current_monitor_status",
        "get_projects_health",
        "get_projects_monitor_health",
        "get_project_statistics",
        // ── Backups: metadata only (NOT s3 credentials/source) ──
        "get_backup",
        "get_backup_schedule",
        "list_backup_schedules",
        "list_backups_for_schedule",
        "list_external_service_backups",
        "list_source_backups",
        "list_schedule_runs",
        "list_restore_runs_for_service",
        "get_restore_capabilities",
        // ── Audit trail ──
        "list_audit_logs",
        "get_audit_log",
        // ── Analytics (the user's own traffic data) ──
        "get_general_stats",
        "get_visitor_stats",
        "get_visitors",
        "get_today_stats",
        "get_recent_activity",
        "get_events_timeline",
        "get_events_count",
        "get_page_paths",
        "get_performance_metrics",
        // Aggregates / counts / booleans — no PII, no secrets
        "get_analytics_events_count",
        "get_event_detail",
        "get_visitor_facets",
        "check_analytics_has_events",
        "get_page_path_detail",
        "get_page_hourly_sessions",
        "get_page_paths_sparklines",
        "get_page_flow",
        "has_analytics_events",
        "get_event_type_breakdown",
        "get_active_visitors",
        "get_hourly_visits",
        "get_unique_counts",
        "get_aggregated_buckets",
        "get_dashboard_projects_analytics",
        "get_metrics_over_time",
        "get_grouped_page_metrics",
        "has_performance_metrics",
        "get_funnel_metrics",
        "list_funnels",
        "get_unique_events",
        // ── API traffic: privacy-safe investigation ──
        // The time series contains only bounded counts and latency/error
        // aggregates. Keep get_api_summary out because refresh=true can incur
        // paid provider work. Keep get_api_routes and get_api_callers out of
        // the general tool loop: paths are attacker-controlled and caller
        // addresses are personal data.
        "get_api_timeseries",
        // Per-visitor/session metadata — same risk class as
        // get_visitors/get_visitor_stats above (IP, geolocation,
        // user_agent, is_crawler, custom_data/event_data)
        "get_event_visitors",
        "get_visitor_details",
        "get_visitor_info",
        "get_analytics_visitor_sessions",
        "get_visitor_journey",
        "get_session_details",
        "get_session_events",
        "get_page_path_visitors",
        "get_analytics_active_visitors",
        "get_live_visitors_list",
        "get_visitor_by_id",
        "get_visitor_by_guid",
        // Excluded on purpose: get_property_breakdown /
        // get_property_timeline (developer custom-property VALUES
        // by name — held pending explicit review), get_session_logs
        // (raw request/response headers), and all
        // temps-analytics-session-replay reads (raw DOM/keystroke
        // capture) — see allowlist comment above.
        //
        // ── Auth / API keys / OIDC (masked keys/secrets only) ──
        "list_api_keys",
        "get_api_key",
        "list_public_providers",
        "list_oidc_providers",
        "list_oidc_provider_users",
        "list_oidc_role_mappings",
        "get_current_user",
        // ── Webhooks (no signing secrets — those are excluded) ──
        "list_webhooks",
        "get_webhook",
        "list_deliveries",
        "get_delivery",
        "list_event_types",
        // ── Data browser: schema navigation, plus opt-in row reads ──
        // Schema shape only (database/schema/bucket names, table and column
        // names, row counts, sizes). These carry no stored values, so they
        // are safe on the same footing as the rest of this list — and they
        // are what lets the agent resolve a question like "the users in the
        // landing production database" to a concrete container path.
        "check_explorer_support",
        "list_root_containers",
        "list_containers_at_path",
        // The data browser's container-info endpoint is now published as
        // `get_query_container_info` (see its `operation_id` in
        // temps-providers). It used to share `get_container_info` with the
        // Docker container endpoint above, and since utoipa keys the document
        // by operation_id one silently overwrote the other — the data browser
        // won, so this allowlist granted the agent that endpoint while the
        // comment here claimed the Docker one. Not listed: navigation is
        // already covered by `list_containers_at_path`, and it is not needed.
        // Entity names. On SQL and MongoDB these are tables and collections —
        // developer-chosen names, no stored values, safe on the same footing as
        // the rest of this list. On Redis and S3 the entity name IS user data
        // (keys embed session tokens and emails; object names are user-supplied
        // filenames), so for those engines the handler gates this behind the
        // same `ai_data_access` opt-in as row reads. Allowlisting it here only
        // makes the endpoint reachable. See `entity_names_are_user_data` in
        // temps-providers.
        "list_entities",
        "get_entity_info",
        // Row CONTENTS. Unlike every other entry here, this one *can* return
        // secrets — password hashes, API tokens, customer PII — because it
        // returns whatever the operator stored. It is therefore gated a
        // second time inside the handler by the per-service `ai_data_access`
        // column, which defaults to false: allowlisting it here only makes
        // the endpoint reachable, it does not grant access to any service.
        // See `read_entity_rows` in temps-providers.
        "read_entity_rows",
        // ── KV / Blob: status only, no connection strings ──
        "kv_status",
        "blob_status",
        "blob_list",
        // ── Email: stats/tracking/DNS, no provider credentials ──
        "list_emails",
        "get_email",
        "get_email_stats",
        "list_email_domains",
        "get_global_event_stats",
        "get_global_events",
        "get_email_tracking",
        "get_email_events",
        "get_email_links",
        // ── Git: provider/connection/repo metadata, no OAuth tokens ──
        "list_git_providers",
        "get_git_provider",
        "list_connections",
        "list_repositories_by_connection",
        "list_repositories_by_provider",
        "list_synced_repositories",
        "get_repository_preset_by_name",
        "get_repository_by_name",
        "get_public_branches",
        "get_public_repository",
        // ── Agents / skills / MCP / sandbox — config + run status ──
        "list_agents",
        "get_agent",
        "list_skills",
        "get_skill",
        "list_mcps",
        "get_mcp",
        "list_global_skills",
        "get_global_skill",
        "list_global_mcps",
        "get_global_mcp",
        "get_run",
        "get_cli_status",
        "get_sandbox_status",
        "get_global_sandbox_status",
        // ── AI Gateway: masked keys, usage/cost stats, conversations ──
        "list_models",
        "get_usage_summary",
        "get_usage_by_provider",
        "get_usage_timeseries",
        "get_usage_top_models",
        "get_usage_recent",
        "get_conversations",
        "get_conversation_detail",
        "get_pricing",
        "list_provider_keys",
        // ── Status page: monitors/incidents/uptime ──
        "list_monitors",
        "get_monitor",
        "get_uptime_history",
        "get_bucketed_status",
        "list_incidents",
        "get_incident",
        "get_incident_updates",
        "get_bucketed_incidents",
        // ── Vulnerability scanning: results, no credentials ──
        "list_project_scans",
        "get_scan",
        "get_scan_vulnerabilities",
        "get_latest_scan",
        "get_latest_scans_per_environment",
        "get_scan_by_deployment",
        // ── Import ──
        "list_sources",
        "get_import_status",
        // ── Container metrics history ──
        // Same class as `get_container_metrics` (already allowlisted); returns
        // time-series data points with no secrets.
        "ContainerMetricsGetHistory",
        // ── Projects: single-project read (metadata, no secrets) ──
        // `get_project` / `get_project_by_slug` mirror `get_projects` but for a
        // specific ID/slug. Both enforce permission_guard + project_scope_guard +
        // project_access_guard — same three-layer check the list endpoint uses.
        "get_project",
        "get_project_by_slug",
        // ── External services: masked env vars only ──
        // `get_service` / `get_service_by_slug` are intentionally NOT included.
        // `get_service` calls `get_service_details`, which runs
        // `mask_sensitive_parameter_values` — a name-heuristic (suffixes like
        // `_key`/`_password`/`_token`/`_secret`, prefixes like `private_`), not
        // an unconditional guarantee. A parameter stored under a non-standard
        // name (e.g. a custom service plugin's `psk`, or a PEM blob under
        // `cert`) passes through unmasked, and the schema's per-parameter
        // `encrypted` flag (externalsvc/mod.rs's `ServiceParameter`) isn't
        // wired into that masking function to catch what the heuristic misses
        // — flagged in security review on PR #732 (Greptile + internal audit).
        // `get_service_by_slug` additionally has no `assert_service_owned_by_caller`
        // check (handlers.rs:2343), so it would let the AI resolve any service
        // on the instance by slug, not just ones linked to the caller's project.
        // The `get_service_environment_variables` bulk endpoint explicitly sets
        // `mask_sensitive: true`; `get_project_service_environment_variables` calls
        // `mask_environment_variable_values` before responding — both unconditional
        // (every value becomes "***", not name-heuristic). Preview env var
        // endpoints return names only or masked values by design.
        "get_service_environment_variables",
        "get_project_service_environment_variables",
        "get_service_preview_environment_variable_names",
        "get_service_preview_environment_variables_masked",
        // Provider type catalog and schema metadata — no stored values.
        "get_provider_metadata",
        "get_providers_metadata",
        // Available Docker images for external services (name, version tags).
        "list_available_containers",
        // AI data-access toggle — boolean (enabled/disabled) per service.
        "get_ai_data_access",
        // Slow-query statistics from pg_stat_statements — parameterised query
        // text ($1/$2 placeholders), execution counts, timing. No data values.
        "get_slow_queries",
        // ── AI traffic analytics (proxy-log aggregates, no raw rows) ──
        // These endpoints return aggregated breakdowns and time-series counts
        // derived from proxy logs — agent names, page paths, HTTP status buckets,
        // request counts, latencies. No full request/response bodies are included.
        "get_ai_agent_breakdown",
        "get_ai_agent_pages",
        "get_ai_agent_timeline",
        "get_ai_page_breakdown",
        "get_ai_status_breakdown",
        // Time-bucketed request aggregates (counts, error rates, p50/p95 latency).
        // Same privacy class as `get_api_timeseries` (already allowlisted).
        "get_time_bucket_stats",
        // Proxy route table — hostname→environment mapping, no credentials.
        "list_routes",
        "get_route",
        // ── OpenTelemetry: cross-project traces, span stats ──
        // `getCrossProjectTraceSiblings` requires OtelRead + denies deployment
        // tokens; its response (`CrossProjectTraceResponse`) is genuinely
        // metadata-only — project id/name/slug/first_seen timestamp, no span
        // content. `getUnifiedTrace` is intentionally NOT included: it embeds
        // the full `SpanRecord` per span, whose `attributes` field is
        // documented as "raw key/value pairs exactly as reported by the
        // instrumenting library" plus raw `events` — same risk class as the
        // already-excluded observability span attributes, but fanned out
        // across up to 20 projects instead of one. Flagged in security review
        // on PR #732 (Greptile, 5th pass).
        "getCrossProjectTraceSiblings",
        // Span statistics ranked by latency/volume (aggregates, no span payloads).
        "query_span_stats",
        // ── Alarms ──
        // Project-scoped alarm list and summary counts; permission_guard DeploymentsRead.
        "listProjectAlarms",
        "getProjectAlarmsSummary",
        // ── Observability event store: excluded entirely ──
        // Neither `observability_list_events` nor `observability_full_event`
        // is included. `_full_event` returns the un-truncated row outright
        // (for errors, `FullError.data` — "the full JSONB blob... stack
        // trace, breadcrumbs, request context, everything", service.rs).
        // `_list_events`'s "truncated" preview rows are NOT safe either:
        // `truncate_stacktrace`/the attributes preview (types.rs) only cap
        // *count* (first 5 stack frames, first 20 span attribute keys) —
        // they don't redact *content*. So the list endpoint still returns,
        // verbatim: `RequestRow.query_string` (untruncated — can carry
        // `?token=`/`?api_key=`/PII), `SpanRow.attributes` (raw
        // developer-set tags — same risk class as the already-excluded
        // `get_property_breakdown`), `ErrorRow.stacktrace_preview` (raw
        // frames), and `ErrorRow.message`. Only `request_headers`/
        // `response_headers` are genuinely safe (allowlist-filtered by
        // `HEADER_WHITELIST`). Flagged in security review on PR #732
        // (Greptile, 3rd pass).
        // ── IP access control ──
        // Access-rule list and single-IP block check; no secrets.
        "check_ip_blocked",
        "get_ip_access_control",
        "list_ip_access_control",
        // Geolocation lookup for an IP — country/city, no PII beyond what the
        // caller already knows (the IP itself). Gated by AnalyticsRead.
        "get_ip_geolocation",
        // ── AI chat: conversations + pending actions ──
        // Conversations are the operator's own chat history. Pending actions are
        // proposed (not yet executed) write operations waiting for confirmation.
        // Both are scoped to the current user's projects via ProjectsRead.
        "get_conversation",
        "find_conversation",
        "list_conversations",
        "list_all_conversations",
        "get_pending_action",
        "list_pending_actions",
        // Instance AI-provider readiness; project access is enforced separately.
        "get_chat_readiness",
        // ── OTel dashboards ──
        // Dashboard config (queries, panel layout) — no secrets. Gated OtelRead.
        "get_dashboard",
        "list_dashboards",
        // ── Platform info + update status ──
        // OS type, architecture, and supported platform strings — no secrets.
        "get_platform_info",
        // Which subsystems this process actually provides (serve profile plus a
        // boolean per capability). No secrets, and it is what lets the assistant
        // answer "why can't I create a database here?" with the real reason
        // instead of guessing from a failed call.
        "get_platform_features",
        // Server's externally-reachable public IP as detected at startup.
        "get_public_ip",
        // Whether an in-place binary update is possible and what version is available.
        "get_update_capability",
        "get_update_status",
        // ── Email provider (masked credentials) + tracking ──
        // `get_email_provider` / `list_email_providers` call `get_masked_credentials`
        // before responding — the actual SMTP/SES credentials are replaced with "***".
        "get_email_provider",
        "list_email_providers",
        // Whether email sending is configured (boolean). Unauthenticated endpoint.
        "email_status",
        // Tracking pixel / click-through status booleans — not the raw events.
        "get_email_tracking_status",
        // ── DNS providers (masked credentials) + domain orders ──
        // `get_dns_provider` / `list_dns_providers` call `get_masked_credentials`
        // before responding — the actual provider API key is replaced with "***".
        "get_dns_provider",
        "list_dns_providers",
        // Domain lookups — metadata only, no signing secrets.
        "get_domain_by_host",
        "get_domain_by_id",
        "get_domain_order",
        "list_orders",
        "list_on_demand_certs",
        // DNS provider zone list (zone IDs and names for domain selection).
        "list_provider_zones",
        // Live DNS A-record lookup for a hostname — diagnostic read.
        "lookup_dns_a_records",
        // Flat vs. wildcard subdomain preview mode for DNS providers.
        "preview_hostname_mode",
        // ── User preferences + cluster metadata ──
        // Notification preferences for the current user — no credentials.
        "get_preferences",
        // Boolean: whether a cluster join token is set (not the token itself).
        "get_join_token_status",
        // Enrollment token metadata (expiry, use count, bound node). Token values
        // are never returned; the response only contains `EnrollmentTokenInfo`.
        "list_enrollment_tokens",
        // Static list of all available permission names — not sensitive.
        "get_api_key_permissions",
        // ── Teams + project access ──
        // Team and membership metadata scoped by UsersRead permission.
        "list_teams",
        "get_team",
        "list_team_members",
        "list_team_projects",
        "list_project_access",
        // ── Agent secrets + AI provider status ──
        // Agent secrets always return value masked as "***" in responses.
        "list_secrets",
        // Project secrets — metadata only (key names + environment scoping).
        // Values are never returned by this endpoint; see its doc comment.
        "listProjectSecrets",
        // AI provider list — reports installed/authenticated booleans and
        // available models; actual credentials are never included.
        "list_ai_providers",
        // Known AI agent identifiers (name catalogue, no credentials).
        "list_known_ai_agents",
        // ── Feature flags ──
        "get_flag",
        "get_flag_snapshot",
        "list_flags",
        // ── Project presets + templates ──
        // Built-in and community preset catalogue — no secrets.
        "list_presets",
        "get_project_template",
        "list_project_templates",
        "list_project_template_tags",
        // Detect applicable presets from a public repository URL.
        // Makes read-only calls to public git provider APIs (same as
        // `get_public_branches` / `get_public_repository` already allowlisted).
        "detect_public_presets",
        // ── Static files + source maps + releases (error tracking) ──
        // Error-tracking release metadata (version strings, file lists).
        // No stored values; same permission class as list_error_groups.
        "list_releases",
        "list_release_files",
        "list_static_bundles",
        "get_static_bundle",
        "list_source_maps",
        "list_source_files",
        // Remote external image metadata (registry ref + description). No credentials.
        "get_remote_external_image",
        "list_remote_external_images",
        // ── Deployment tokens (masked prefix only) ──
        // `DeploymentTokenResponse` exposes only `token_prefix` (first few chars),
        // not the full token — same masking class as `get_api_key` (already listed).
        "get_deployment_token",
        "list_deployment_tokens",
        // ── Backup metadata ──
        // Alert thresholds and child backup records — no S3 credentials (those
        // are in `get_s3_source` / `list_s3_sources`, which are excluded).
        "list_backup_alerts",
        "list_backup_children",
        "list_schedule_run_jobs",
        "list_service_schedules",
        "list_schedule_services",
        // ── PostgreSQL major-version upgrade status ──
        "get_pg_upgrade",
        "get_pg_upgrade_logs",
        "list_pg_upgrades",
        // ── Git: repository / branch / tag reads ──
        // Boolean safety check (can_delete + projects_in_use list).
        "check_provider_deletion_safety",
        // Boolean: whether a specific commit SHA exists in a repository.
        "check_commit_exists",
        // Repository and ref metadata; no OAuth tokens. Same class as
        // `list_repositories_by_connection` already allowlisted.
        "get_all_repositories_by_name",
        "get_repository_by_id",
        "get_branches_by_repository_id",
        "get_tags_by_repository_id",
        "list_commits_by_repository_id",
        "get_repository_branches",
        "get_repository_tags",
        "get_repository_preset_live",
        // Git connection metadata (connection IDs, names, provider type).
        "get_provider_connections",
        // ── Log aggregator: context window — excluded ──
        // `get_log_context` is intentionally NOT included. It returns raw
        // `message`/`fields` from application stdout/stderr (arbitrary
        // developer-controlled content), and its resolver
        // (temps-log-aggregator/src/services/search.rs `get_context`) has NO
        // project-ownership check on `chunk_id` at all — only a global
        // `LogsRead` permission gate, unlike `get_container_logs`'s
        // container-scoped equivalent. Flagged in security review on PR #732
        // (Greptile, 4th pass).
        // ── Preview gateway: settings + status (NOT logs) ──
        // Settings expose image tag, host port, auto-upgrade flag — no shared_secret
        // (that is masked as a boolean in the response struct).
        // `get_preview_gateway_logs` is intentionally NOT included: it
        // returns `LogsResponse { lines }` straight from `tail_logs` —
        // raw Docker container stdout/stderr, zero redaction. Flagged in
        // security review on PR #732 (Greptile, 5th pass).
        "get_preview_gateway_settings",
        "get_preview_gateway_status",
        // ── External plugins ──
        "list_external_plugins",
        // ── Analytics: event entry list — excluded ──
        // `get_event_entries` is intentionally NOT included. Per its own doc
        // comment: "raw occurrences of a specific event, including custom
        // JSON properties" — same risk class as the already-excluded
        // `get_property_breakdown`/`get_property_timeline`. Flagged in
        // security review on PR #732 (Greptile, 4th pass).
        // ── Agents + sandbox: run status, job metadata ──
        // `list_agent_runs`, `list_all_runs`, `get_run_with_logs`, and
        // `latest_run_for_source` are intentionally NOT included: they all
        // serialize `AgentRunResponse`, which carries `ai_output`,
        // `ai_reasoning`, `prompt_text`, `ephemeral_yaml`, `analysis`, and
        // `user_context` verbatim — raw agent execution content, not
        // metadata. `job_status` is also excluded: `JobStatusResponse`
        // returns raw `stdout`/`stderr` from a detached sandbox command.
        // `list_jobs`/`get_cmd` are ALSO now excluded (previously kept —
        // wrong call): their DTOs (`JobSummaryResponse.cmd`,
        // `CmdInner.args`) carry the full invoked command line, which can
        // itself embed a secret passed as a CLI argument (e.g. `mysql
        // -p'...'`, `curl -H "Authorization: Bearer ..."`). Flagged in
        // security review on PR #732 (Greptile, 4th + 5th pass).
        "get_sandbox",
        "list_sandboxes",
        // Sandbox event list — lifecycle events (created, started, stopped).
        "list_events",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Mutating operations the AI may invoke through the native harness policy.
///
/// `temps_write` records an encrypted pending action, applies the conversation's
/// approval mode, and replays approved mutations through the same router
/// (`permission_guard!` + audit). Destructive operations always remain explicit.
///
/// Conservative by design: high-value, mostly-reversible lifecycle + config
/// operations. Adding an entry is a product + security decision — what may the
/// AI propose for a human to run.
///
/// Extracted into its own function (rather than an inline literal at the call
/// site) so tests can assert it stays disjoint from `ai_read_safe_posts()`,
/// which is the one rule holding up the read-only-POST mechanism.
fn ai_write_allowlist() -> Vec<String> {
    [
        // ── Deployment lifecycle (reversible / safe) ──
        // Redeploy the project from its configured branch —
        // what a "redeploy main" request maps to
        // (promote/rollback are NOT redeploys).
        "trigger_project_pipeline",
        "rollback_to_deployment",
        "promote_deployment",
        "pause_deployment",
        "resume_deployment",
        "cancel_deployment",
        // ── Manual image deploy (no git build) ──
        // Deploy a prebuilt Docker image by `image_ref` (a
        // pullable registry ref) or a registered
        // `external_image_id`, to a specific environment_id.
        // Static-bundle deploys are intentionally NOT here: the
        // AI can't perform the multipart file upload, so the
        // whole static flow (upload + deploy) lives in the
        // frontend.
        "deploy_from_image",
        // ── Container runtime control (reversible) ──
        "restart_container",
        "stop_container",
        "start_container",
        // ── Environment wake/sleep (reversible) ──
        "wake_environment",
        "sleep_environment",
        // ── Environment settings (resource limits, replicas,
        //    branch) — what "raise memory to 512 MB" /
        //    "give it more CPU" / "scale to 2 replicas" map to.
        //    Values are microcores (1_000_000 = 1 core) and MB.
        //    Reversible: it's a config change, re-applicable.
        "update_environment_settings",
        // ── Automatic deployment + Git delivery repairs ──
        // These are the smallest reversible fixes for the common "pushes do
        // not deploy" workflow. `update_git_settings` and webhook reinstall
        // can contact the configured Git provider, but `temps_write` only
        // stages the exact request: the current user must still have the
        // operation's permission and the harness policy must authorize replay.
        "update_automatic_deploy",
        "update_git_settings",
        "reinstall_gitlab_webhook",
        // ── Environment variables (set / change) ──
        "create_environment_variable",
        "update_environment_variable",
        "delete_environment_variable",
        // ── Domains (attach / detach at the environment level only;
        //    account-global domain create/delete excluded) ──
        "add_environment_domain",
        "delete_environment_domain",
        // ── Managed external services (databases, caches, etc.) —
        //    provisioning a new container and linking an existing
        //    one to a project. Both reversible (a service can be
        //    unlinked / left running unused; nothing is deleted).
        "create_service",
        "link_service_to_project",
        // ── AI application topology ──
        // Composite create is the only safe way for the model to create a
        // Temps project and immediately link its generated id to the current
        // application. Each operation is still replayed with current user
        // authorization after native approval.
        "create_application_project",
        "deploy_application_workspace_project",
        "link_application_project",
        "unlink_application_project",
        "set_application_primary_project",
        "update_application_workspace",
        "control_application_workspace",
        // ── Metric alert rules (OTel) ──
        // Create/update an alert rule. Reversible (a rule
        // can be disabled or deleted) and non-destructive:
        // creating one changes no running workload, it only
        // starts evaluating a metric. This is what lets the
        // assistant turn "your p95 has no alert on it" into
        // a concrete rule the human confirms.
        //
        // `delete_alert` is deliberately excluded: deleting
        // an alert silently removes monitoring, which is the
        // kind of change that is only noticed when the
        // incident it would have caught happens.
        "create_alert",
        "update_alert",
        // ── Global notification control plane ──
        // Provider payloads may contain credentials. `temps_write` encrypts
        // executable parameters at rest and redacts both the approval card and
        // result returned to the model. DELETE operations remain explicit even
        // in Auto mode through `platform_request_is_destructive`.
        "create_notification_provider",
        "update_notification_provider",
        "delete_notification_provider",
        "create_slack_provider",
        "update_slack_provider",
        "create_notification_email_provider",
        "update_notification_email_provider",
        "create_webhook_provider",
        "update_webhook_provider",
        "create_cloudflare_provider",
        "update_cloudflare_provider",
        "test_notification_provider",
        "create_notification_route",
        "update_notification_route",
        "delete_notification_route",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Vetted **read-only `POST`** operations for the AI read tool.
///
/// HTTP method is this codebase's structural proxy for "does this mutate?", and
/// `ai_read_allowlist` above is GET-only for exactly that reason. This is the
/// narrow, separately-reviewed exception: operations that are genuinely
/// side-effect-free but are `POST` because their input is a structured document
/// rather than a handful of query params.
///
/// **The rule for adding an entry: the operation must write nothing.** Not a
/// row, not a file, not a queued job. If it mutates anything at all it belongs
/// in the propose-then-confirm write allowlist instead, never here. Keeping this
/// list separate from the GET allowlist is what makes each addition a conscious
/// decision rather than a line lost in a 200-entry list.
///
/// Deliberately NOT here: anything that creates, updates, deletes, triggers, or
/// enqueues — including the metric-alert CRUD operations that live next to
/// `preview_alert` in the same handler module.
fn ai_read_safe_posts() -> Vec<String> {
    [
        // Backtest a metric-alert detector over historical data: replays the
        // metric against the band the evaluator would use and returns which
        // points would have fired. Explicitly documented read-only, guarded by
        // OtelRead + project_access_guard!, and persists nothing. It is a POST
        // only because the request body is a whole detector config.
        //
        // This is what lets the assistant check "would this rule actually have
        // fired?" *before* proposing it, so a suggested alert arrives with
        // evidence attached instead of a guessed threshold.
        "preview_alert",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Initialize and start the console API server
pub async fn start_console_api(params: ConsoleApiParams) -> anyhow::Result<()> {
    let ConsoleApiParams {
        db,
        config,
        cookie_crypto,
        encryption_service,
        route_table,
        queue,
        ready_signal,
        additional_templates,
        on_demand_waker,
        extra_plugins,
        admin_gate_service: provided_admin_gate_service,
        admin_gate_handle: provided_admin_gate_handle,
        retention_resolver_slot,
        project_ip_gate_slot,
        request_policy_gate_slot,
        update_status,
        self_updater,
        traefik_discovery,
        external_plugin_registry,
        profile,
    } = params;

    // Count panics for the anonymous `error_summary` telemetry event. Only
    // the sanitized source location (crate-relative file:line) is recorded —
    // never the panic message, which can embed user data. Chains to the
    // previous hook so normal backtrace printing is unaffected. Task panics
    // don't kill the process, so they are flushed by the summary task below;
    // a fatal main-thread panic may be lost, which is acceptable for v1.
    {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            temps_core::error_metrics::record_panic(panic_info.location());
            previous_hook(panic_info);
        }));
    }

    // Readiness flag for the `/readyz` probe. Starts `false` (not ready) and is
    // flipped to `true` immediately before the listeners begin serving — after
    // routers, middleware, and the admin gate are all built, not merely after
    // plugin init. The health router (mounted on the public surface below)
    // reads this so a supervisor or the split-topology upgrade gate can tell
    // "warming up" (503) from "serving" (200) for the process's lifetime.
    //
    // This is deliberately a *later* point than `ready_signal` below now fires
    // at (see that call site) — `/readyz` needs "actually able to serve a
    // request", `ready_signal` needs only "plugin two-phase init is done".
    let ready_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // PRE-VALIDATE all plugin dependencies BEFORE initializing plugin manager
    // This ensures clear error messages if any critical resources are missing
    debug!("Pre-validating plugin dependencies...");

    // 1. Validate Docker connectivity.
    //
    // In the `full` profile this is fatal and unchanged: that profile's entire
    // job is running containers here, so a missing daemon is a
    // misconfiguration the operator must see immediately.
    //
    // In `control-plane` the daemon is genuinely optional — the process runs
    // no workloads — so an unreachable one is reported at info level and
    // startup continues with no handle registered. Reachability is probed with
    // a real `ping`, not just client construction: a bollard client is a lazy
    // descriptor that "connects" successfully to a socket that does not exist,
    // and reporting `docker: true` on that basis would be a lie.
    debug!("Checking Docker daemon connectivity...");
    let (docker_handle, docker_available) = if profile.local_workloads_enabled() {
        // `full`: a daemon is this profile's entire job, so a missing one is a
        // misconfiguration the operator must see immediately. Probe it for
        // real rather than trusting client construction — a bollard client is
        // a lazy descriptor that "connects" successfully to a socket path
        // that does not exist, so construction alone proves nothing and
        // reporting `docker: true` on that basis would be a lie the console
        // shows to the operator.
        let client = bollard::Docker::connect_with_defaults()
            .map_err(|e| docker_unavailable_error(&e.to_string()))?;

        match tokio::time::timeout(DOCKER_PROBE_TIMEOUT, client.ping()).await {
            Ok(Ok(_)) => debug!("✓ Docker daemon is accessible"),
            Ok(Err(e)) => return Err(docker_unavailable_error(&e.to_string())),
            Err(_) => {
                return Err(docker_unavailable_error(&format!(
                    "the daemon did not answer a ping within {}s",
                    DOCKER_PROBE_TIMEOUT.as_secs()
                )))
            }
        }

        (temps_core::DockerHandle::available(Arc::new(client)), true)
    } else {
        // `control-plane`: no client is constructed at all. Not "constructed
        // and unused" — constructed. This profile is designed to run in a
        // container with no Docker socket and no DOCKER_HOST, where
        // `connect_with_defaults()` itself fails, and where any probe would
        // only produce a guaranteed error line on every boot. Every
        // daemon-dependent path resolves this handle and fails with a typed
        // `DockerUnavailable` naming the remedy.
        info!(
            profile = profile.as_str(),
            "Local workloads are disabled: no Docker client is constructed and no daemon is \
             contacted. Applications run on worker nodes joined with `temps join`; see \
             GET /api/platform/features"
        );
        (
            temps_core::DockerHandle::disabled(
                profile.as_str(),
                temps_core::CONTROL_PLANE_DOCKER_REASON,
            ),
            false,
        )
    };
    let docker_handle = Arc::new(docker_handle);

    // 2. Validate GeoPlugin dependencies (GeoLite2 database)
    debug!("Checking GeoLite2 database...");
    let geo_db_path = config.data_dir.join("GeoLite2-City.mmdb");
    // The license key is an admin setting on the `settings` row, not an env
    // var, so a startup download uses whatever the admin configured through
    // the UI -- the same value the scheduled refresh job reads each tick.
    let maxmind_license_key = configured_maxmind_license_key(&db, &encryption_service).await;
    validate_geolite2_database(&geo_db_path, maxmind_license_key.as_deref()).await?;
    debug!("✓ GeoLite2 database file found");

    // 3. Validate logs directory is writable
    debug!("Checking logs directory...");
    let logs_dir = config.data_dir.join("logs");
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        return Err(anyhow::anyhow!(
            "❌ Logs directory creation FAILED\n\n\
            Cannot create or access the logs directory.\n\n\
            Path: {}\n\
            Error: {}\n\n\
            Solutions:\n\
            1. Check directory permissions\n\
               - Ensure write permissions to parent directory: {}\n\n\
            2. Verify disk space\n\
               - Run: df -h\n\n\
            3. Check file ownership\n\
               - Run: ls -la {}\n\n\
            Logs are required for system diagnostics and operation tracking.",
            logs_dir.display(),
            e,
            config.data_dir.display(),
            config.data_dir.display()
        ));
    }
    debug!("✓ Logs directory is accessible");

    debug!("✓ All plugin dependencies validated successfully");

    // Initialize plugin manager
    let mut plugin_manager = PluginManager::new();

    // Register core services that plugins can access
    let service_context = plugin_manager.service_context();
    service_context.register_service(db.clone());
    service_context.register_service(encryption_service.clone());
    service_context.register_service(cookie_crypto.clone());
    // The Docker client handle is ALWAYS registered; the daemon behind it is
    // not always there. Plugins resolve the handle with `require_service` (it
    // genuinely always exists) and then make the *daemon* optional at the
    // point of use, so a control plane with no socket never panics inside
    // `require_service::<bollard::Docker>()`.
    service_context.register_service(docker_handle.clone());
    // The raw client stays registered too, but only when one exists, so any
    // consumer that has not been migrated to the handle fails the boot-time
    // `verify_required_services` check with a readable error naming itself
    // rather than panicking half-way through initialization.
    if let Some(client) = docker_handle.cloned() {
        service_context.register_service(client);
    }
    // The single boot-time answer to "may this process run workloads?".
    // Registered before any plugin runs so `register_services` can consult it.
    let local_workload_policy = Arc::new(if profile.local_workloads_enabled() {
        temps_core::LocalWorkloadPolicy::full(docker_available)
    } else {
        temps_core::LocalWorkloadPolicy::control_plane(docker_available)
    });
    service_context.register_service(local_workload_policy.clone());
    // Pre-registered here (rather than left solely to AuthPlugin, which also
    // registers an equivalent instance) because TeamsPlugin, GitPlugin,
    // DomainsPlugin, and DeploymentsPlugin all gate sensitive mutations via
    // `require_sensitive_action` and are registered before AuthPlugin in the
    // ordered list below. Only depends on `db`, so it's safe to construct
    // this early.
    let sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer> = Arc::new(
        temps_auth::DefaultSensitiveActionAuthorizer::new(db.clone()),
    );
    service_context.register_service(sensitive_action_authorizer);
    // Background DNS mutation is fail-closed until an optional policy plugin
    // claims this slot. DomainsPlugin captures the slot before later plugins
    // register, so the indirection must exist before plugin initialization.
    let dns_automation_gate_slot = Arc::new(temps_core::DnsAutomationGateSlot::new());
    service_context.register_service(dns_automation_gate_slot);
    // Pre-registered before any plugin runs so ProxyPlugin uses this exact
    // slot instance instead of creating its own — see the field doc on
    // `ConsoleApiParams::retention_resolver_slot`.
    service_context.register_service(retention_resolver_slot.clone());
    // Same pre-registration reasoning as retention_resolver_slot above —
    // see the field doc on `ConsoleApiParams::project_ip_gate_slot` (ADR
    // 0022) for why this is flagged for security review rather than a
    // routine addition.
    service_context.register_service(project_ip_gate_slot.clone());
    service_context.register_service(request_policy_gate_slot.clone());
    // Update-notifier slot: the background loop in serve/mod.rs writes into
    // it; ConfigPlugin's `GET /settings/update-status` reads it so the web
    // console can render the upgrade banner.
    service_context.register_service(update_status.clone());
    // Registered behind the trait so temps-config depends only on the
    // temps-core contract, never on the CLI crate that implements it.
    service_context.register_service(self_updater.clone() as Arc<dyn temps_core::SelfUpdater>);
    // Traefik label discovery status, resolved in serve/mod.rs. Pre-registered
    // before any plugin runs so DeploymentsPlugin's `/traefik-discovery/*`
    // handlers report this process's real state instead of falling back to a
    // handle rebuilt from the environment.
    service_context.register_service(traefik_discovery.clone());

    // Register the shared route table (created in serve/mod.rs)
    // This is used by analytics-events and other plugins that need to resolve hosts
    // Note: Route table listener is started in serve/mod.rs to avoid duplicate listeners
    service_context.register_service(route_table.clone());
    service_context.register_service(
        route_table.clone() as Arc<dyn temps_core::route_table::RouteTableRefresher>
    );

    // Register TemplateService - provides project templates from YAML configuration
    // Bundled templates are loaded automatically; external file in data_dir can override them
    let templates_override_path = config.data_dir.join("templates.yaml");
    let template_service = Arc::new(TemplateService::new(Some(templates_override_path))?);

    // Load additional template files if specified
    for additional_path in &additional_templates {
        info!("Loading additional templates from {:?}", additional_path);
        if let Err(e) = template_service.load_additional(additional_path).await {
            return Err(anyhow::anyhow!(
                "❌ Failed to load additional templates from {:?}\n\n\
                Error: {}\n\n\
                Please check the file exists and contains valid YAML with valid services.\n\
                Valid services are: {}",
                additional_path,
                e,
                temps_core::templates::VALID_SERVICES.join(", ")
            ));
        }
    }

    service_context.register_service(template_service);

    // Register OnDemandWaker so environment wake/sleep endpoints can manage containers
    if let Some(waker) = on_demand_waker {
        service_context.register_service(waker as Arc<dyn temps_core::OnDemandWaker>);
        debug!("Registered OnDemandWaker for environment wake/sleep endpoints");
    }

    // Whether this process runs workloads on its own host. Plugins that exist
    // only to manage local containers are not constructed at all when it is
    // false: an unconstructed plugin holds no services, spawns no background
    // loops, and costs nothing. Skipping is deliberately limited to plugins
    // whose services no kept plugin requires — the startup requirement check
    // in `PluginManager::initialize_plugins` turns any mistake here into one
    // readable error instead of a panic during initialization.
    let local_workloads = profile.local_workloads_enabled();
    let mut skipped_plugins: Vec<&'static str> = Vec::new();

    // Register plugins in dependency order:
    // 1. ConfigPlugin - provides configuration services
    debug!("Registering ConfigPlugin");
    let config_plugin = Box::new(ConfigPlugin::new(config.clone()));
    plugin_manager.register_plugin(config_plugin);

    // Optional managed control plane. It owns the enrollment state and the
    // background telemetry mirror consumed later by OtelPlugin.
    debug!("Registering CloudPlugin");
    let cloud_plugin = Box::new(CloudPlugin::new(
        config.data_dir.clone(),
        env!("CARGO_PKG_VERSION"),
    ));
    plugin_manager.register_plugin(cloud_plugin);

    // 1.5. TelemetryPlugin - registers the anonymous telemetry reporter
    // (depends only on ServerConfig for the data dir). Registered early so
    // every later plugin can require the Arc<dyn TelemetryReporter>.
    debug!("Registering TelemetryPlugin");
    // TEMPS_VERSION (git-describe, set by build.rs) is used instead of
    // CARGO_PKG_VERSION so nightly/beta builds report a version telemetry
    // can actually distinguish from a tagged release -- CARGO_PKG_VERSION
    // is the static Cargo.toml version and is identical across all of them.
    let telemetry_plugin = Box::new(TelemetryPlugin::new(config.clone(), env!("TEMPS_VERSION")));
    plugin_manager.register_plugin(telemetry_plugin);

    // 2. QueuePlugin - registers the pre-created job queue into the service context
    debug!("Registering QueuePlugin");
    let queue_plugin = Box::new(QueuePlugin::new(queue));
    plugin_manager.register_plugin(queue_plugin);

    // 2.5. LogsPlugin - provides logging services (no dependencies)
    //
    // Resolved once here and reused for LogAggregatorPlugin below: one
    // TEMPS_LOG_STORAGE_BACKEND / TEMPS_LOG_S3_* operator decision drives
    // where Temps puts BOTH build/deploy job logs (this plugin) and
    // aggregated container logs (LogAggregatorPlugin) -- they're the same
    // "local disk or S3-compatible bucket" question, so they share one
    // config resolution instead of two independent env-var reads that could
    // drift out of sync.
    debug!("Registering LogsPlugin");
    let logs_dir = config.data_dir.join("logs");
    let shared_log_storage_config = log_aggregator_storage_config(&config.data_dir)
        .map_err(|e| anyhow::anyhow!("❌ Log storage configuration is invalid\n\n{e}"))?;
    let logs_plugin = Box::new(LogsPlugin::new(logs_dir, shared_log_storage_config.clone()));
    plugin_manager.register_plugin(logs_plugin);

    // 3.1. EventsPlugin - provides custom events tracking (depends on database)
    debug!("Registering EventsPlugin");
    let events_plugin = Box::new(EventsPlugin);
    plugin_manager.register_plugin(events_plugin);

    // 3.2. FunnelsPlugin - provides funnel analytics (depends on database)
    debug!("Registering FunnelsPlugin");
    let funnels_plugin = Box::new(FunnelsPlugin);
    plugin_manager.register_plugin(funnels_plugin);

    // 3.3. SessionReplayPlugin - provides session replay (depends on database)
    debug!("Registering SessionReplayPlugin");
    let session_replay_plugin = Box::new(SessionReplayPlugin);
    plugin_manager.register_plugin(session_replay_plugin);

    // 3.4. PerformancePlugin - provides performance metrics (depends on database)
    debug!("Registering PerformancePlugin");
    let performance_plugin = Box::new(PerformancePlugin);
    plugin_manager.register_plugin(performance_plugin);

    // 4. GeoPlugin - provides geolocation services (database validated in pre-validation)
    debug!("Registering GeoPlugin");
    let geo_plugin = Box::new(GeoPlugin::new());
    plugin_manager.register_plugin(geo_plugin);

    // 3.5. InfraPlugin - provides infrastructure and platform information (no dependencies)
    debug!("Registering InfraPlugin");
    let infra_plugin = Box::new(InfraPlugin::new());
    plugin_manager.register_plugin(infra_plugin);

    // 5. AuditPlugin - provides audit logging (depends on database and geo services)
    debug!("Registering AuditPlugin");
    let audit_plugin = Box::new(AuditPlugin::new());
    plugin_manager.register_plugin(audit_plugin);

    // 5.1. TeamsPlugin - registers project-scoped RBAC. Project-facing
    // plugins capture its ProjectAccessChecker while registering services,
    // so Teams must precede them (and follow AuditPlugin, which it requires).
    debug!("Registering TeamsPlugin");
    let teams_plugin = Box::new(TeamsPlugin::new());
    plugin_manager.register_plugin(teams_plugin);

    // 6. GitPlugin - provides git functionality (depends on other services)
    debug!("Registering GitPlugin");
    let git_plugin = Box::new(GitPlugin::new());
    plugin_manager.register_plugin(git_plugin);

    // 7. NotificationsPlugin - provides notification services (must come before AuthPlugin)
    debug!("Registering NotificationsPlugin");
    let notifications_plugin = Box::new(NotificationsPlugin::new());
    plugin_manager.register_plugin(notifications_plugin);

    // 4. DnsPlugin - provides DNS provider management (depends on database and encryption)
    // Must be registered before DomainsPlugin and EmailPlugin so DnsProviderService is available
    debug!("Registering DnsPlugin");
    let dns_plugin = Box::new(DnsPlugin::new());
    plugin_manager.register_plugin(dns_plugin);

    // 4.5. DomainsPlugin - provides TLS certificate management (depends on config, database, and DnsProviderService)
    debug!("Registering DomainsPlugin");
    let domains_plugin = Box::new(DomainsPlugin::new());
    plugin_manager.register_plugin(domains_plugin);

    // 7.1. EmailPlugin - provides email sending and domain management (depends on database, encryption, and optionally DnsProviderService)
    debug!("Registering EmailPlugin");
    let email_plugin = Box::new(EmailPlugin::new());
    plugin_manager.register_plugin(email_plugin);

    // Must follow EmailPlugin: tracking uses the email schema/services for
    // recipient correlation and domain-scoped bounce suppression.
    let email_tracking_plugin = Box::new(temps_email_tracking::EmailTrackingPlugin::new());
    plugin_manager.register_plugin(email_tracking_plugin);

    // 7.5. WebhooksPlugin - provides webhook delivery and management (depends on database and encryption)
    debug!("Registering WebhooksPlugin");
    let webhooks_plugin = Box::new(WebhooksPlugin::new());
    plugin_manager.register_plugin(webhooks_plugin);

    // 5. ProvidersPlugin - provides external service management (depends on database and encryption)
    debug!("Registering ProvidersPlugin");
    let providers_plugin = Box::new(ProvidersPlugin::new());
    plugin_manager.register_plugin(providers_plugin);

    // 5.1. KvPlugin - provides key-value storage (depends on database, docker).
    // Managed Redis runs as a container on this host, so there is nothing for
    // it to manage without local workloads.
    if local_workloads {
        debug!("Registering KvPlugin");
        let kv_plugin = Box::new(KvPlugin::new());
        plugin_manager.register_plugin(kv_plugin);
    } else {
        skipped_plugins.push("kv");
    }

    // 5.2. BlobPlugin - provides blob storage (depends on database, docker)
    debug!("Registering BlobPlugin");
    let blob_plugin = Box::new(BlobPlugin::new());
    plugin_manager.register_plugin(blob_plugin);

    // 5.3. FlagsPlugin - provides feature flags (depends on database only:
    // flags are control-plane rows, no container and no background task)
    debug!("Registering FlagsPlugin");
    let flags_plugin = Box::new(FlagsPlugin::new());
    plugin_manager.register_plugin(flags_plugin);

    // 5.5. EnvironmentsPlugin - provides environment management (depends on config)
    debug!("Registering EnvironmentsPlugin");
    let environments_plugin = Box::new(EnvironmentsPlugin::new());
    plugin_manager.register_plugin(environments_plugin);

    // 6. ProjectsPlugin - provides project management (depends on providers, config, queue, templates)
    debug!("Registering ProjectsPlugin");
    let projects_plugin = Box::new(ProjectsPlugin::new());
    plugin_manager.register_plugin(projects_plugin);

    // 7. DeployerPlugin - provides container deployment (depends on Docker)
    debug!("Registering DeployerPlugin");
    let deployer_plugin = Box::new(DeployerPlugin::new());
    plugin_manager.register_plugin(deployer_plugin);

    // 7.5. ScreenshotsPlugin - provides screenshot capture services (depends on config)
    debug!("Registering ScreenshotsPlugin");
    let screenshots_plugin = Box::new(ScreenshotsPlugin::new());
    plugin_manager.register_plugin(screenshots_plugin);
    // 8. ErrorTrackingPlugin - provides error tracking and monitoring (includes Sentry ingestion)
    debug!("Registering ErrorTrackingPlugin");
    let error_tracking_plugin = Box::new(ErrorTrackingPlugin::new());
    plugin_manager.register_plugin(error_tracking_plugin);

    // 8.5. VulnerabilityScannerPlugin - provides vulnerability scanning (depends on database and audit)
    //
    // Registered in EVERY profile. Scanning a fresh image requires Trivy
    // against THIS host's local image store, and is gated internally: the
    // scanner holds a `DockerHandle` and returns a typed
    // `ScannerError::DockerUnavailable` from every scan call on a
    // control-plane process (no local daemon), rather than failing plugin
    // registration. Dropping the plugin entirely would also 404 the
    // `/vulnerability-scans` API and lose access to scan history recorded
    // before this instance was reconfigured to `control-plane`, which is a
    // control-plane responsibility, not a local-workload one.
    //
    // Scanning images that live only on a worker node is not implemented in
    // any profile today -- there is no remote scan orchestration over the
    // agent channel. `PlatformFeatures::vulnerability_scanning` is `false`
    // under `control-plane` and that is the honest, complete picture; no
    // code path here silently claims otherwise.
    debug!("Registering VulnerabilityScannerPlugin");
    let vulnerability_scanner_plugin = Box::new(VulnerabilityScannerPlugin::new());
    plugin_manager.register_plugin(vulnerability_scanner_plugin);

    // 8.6. AgentsPlugin - MUST be registered before DeploymentsPlugin so DeploymentsPlugin can
    // resolve AgentSyncService via the plugin context. If registered after, DeploymentsPlugin
    // falls back to NoOpAgentSyncService and agent sync is silently skipped on every deployment.
    // Agent sandboxes ARE local containers. DeploymentsPlugin resolves the
    // AgentSyncService with `get_service` and falls back to a no-op, so
    // skipping this is safe — see the ordering note above.
    if local_workloads {
        debug!("Registering AgentsPlugin");
        let agents_plugin = Box::new(AgentsPlugin::new());
        plugin_manager.register_plugin(agents_plugin);
    } else {
        skipped_plugins.push("agents");
    }

    // 8.7. AI Gateway Plugin - registers the provider-neutral AiService.
    // Application harness turns must see the sandbox provider registered by
    // AgentsPlugin. Registering this earlier snapshots `None` and makes every
    // sandboxed application thread fail closed for the server lifetime.
    debug!("Registering AiGatewayPlugin");
    let ai_gateway_plugin = Box::new(temps_ai_gateway::AiGatewayPlugin::new());
    plugin_manager.register_plugin(ai_gateway_plugin);

    // Analytics depends on the provider-neutral AiService, so it follows the
    // gateway registration rather than relying on an earlier incidental order.
    debug!("Registering AnalyticsPlugin");
    let analytics_plugin = Box::new(AnalyticsPlugin::new());
    plugin_manager.register_plugin(analytics_plugin);

    // 9. DeploymentsPlugin - provides deployment orchestration (depends on deployer, screenshots, and vulnerability scanner)
    debug!("Registering DeploymentsPlugin");
    let deployments_plugin = Box::new(DeploymentsPlugin::new());
    plugin_manager.register_plugin(deployments_plugin);

    // 9.0. McpServerPlugin (ADR-039) - MCP server for AI tool integration.
    // Depends on ProjectsPlugin and DeploymentsPlugin (services registered above).
    // Routes are at root level (not under /api); assembled below after plugin init.
    debug!("Registering McpServerPlugin");
    let mcp_plugin = Box::new(McpServerPlugin::new());
    plugin_manager.register_plugin(mcp_plugin);

    // 8.8. SandboxPlugin - Vercel-compatible `/v1/sandbox/*` API.
    // Consumes the shared SandboxProvider registered by AgentsPlugin.
    // Consumes the SandboxProvider AgentsPlugin registers, so it goes
    // wherever AgentsPlugin goes.
    if local_workloads {
        debug!("Registering SandboxPlugin");
        let sandbox_plugin = Box::new(SandboxPlugin::new());
        plugin_manager.register_plugin(sandbox_plugin);
    } else {
        skipped_plugins.push("sandbox");
    }

    // 9.1. LogAggregatorPlugin - structured log collection, storage, search, and streaming
    // Depends on database, Docker (from DeployerPlugin), and AuditLogger (from AuditPlugin).
    //
    // Registered in EVERY profile. Its local collector tails containers on
    // this host's daemon and is gated internally on the local-workload
    // policy, but `RemoteLogCollectorService` — which collects logs from
    // worker nodes over the agent — and the `/logs/search` and `/logs/tail`
    // routes are exactly what a control plane needs most. Dropping the plugin
    // would 404 those routes and leave worker logs uncollected, which is the
    // opposite of what this profile is for.
    debug!("Registering LogAggregatorPlugin");
    // Reuses `shared_log_storage_config`, resolved once above alongside
    // LogsPlugin -- see the comment there for why these two plugins share a
    // single config resolution instead of two independent env-var reads.
    let log_aggregator_plugin = Box::new(LogAggregatorPlugin::new(shared_log_storage_config));
    plugin_manager.register_plugin(log_aggregator_plugin);

    // 9.5. ImportPlugin - provides workload import functionality (depends on
    // GitPlugin, ProjectsPlugin, DeploymentsPlugin).
    //
    // Every importer inspects containers on the local Docker daemon to adopt
    // an existing Compose / Coolify / Dokploy / Portainer / Kamal / CapRover
    // stack, so there is nothing for it to read here.
    if local_workloads {
        debug!("Registering ImportPlugin");
        let import_plugin = Box::new(ImportPlugin::new());
        plugin_manager.register_plugin(import_plugin);
    } else {
        skipped_plugins.push("import");
    }

    // 9.6. StatusPagePlugin - provides status page and monitoring (depends on database and projects)
    debug!("Registering StatusPagePlugin");
    let status_page_plugin = Box::new(StatusPagePlugin::new());
    plugin_manager.register_plugin(status_page_plugin);

    // 9.7. OtelPlugin - provides OpenTelemetry metrics, traces, and logs collection (depends on database)
    debug!("Registering OtelPlugin");
    let otel_plugin = Box::new(OtelPlugin::new());
    plugin_manager.register_plugin(otel_plugin);

    // 9.8. MonitoringPlugin - registers AlarmService in the service registry and
    // wires the alarms read/ack/resolve HTTP routes (ADR-025 Phase 1).
    // Must be registered AFTER NotificationsPlugin (step 7) and QueuePlugin (step 2)
    // since AlarmService requires both. Must be registered BEFORE the background
    // loops below that consume the registered AlarmService.
    debug!("Registering MonitoringPlugin");
    let monitoring_plugin = Box::new(MonitoringPlugin::new());
    plugin_manager.register_plugin(monitoring_plugin);

    // 10. AuthPlugin - provides authentication and authorization (depends on notification service)
    debug!("Registering AuthPlugin");
    let auth_plugin = Box::new(AuthPlugin::new());
    plugin_manager.register_plugin(auth_plugin);

    // 11. BackupPlugin - provides backup services (depends on database, audit, and notification services, and providers)
    //
    // Registered in EVERY profile. Only backup *execution against a container
    // on this host* depends on a local daemon, and that is gated inside the
    // plugin by the local-workload policy. Scheduling, retention, listing,
    // restore orchestration and backups of services owned by worker nodes are
    // control-plane responsibilities and must keep running here — skipping
    // the plugin would remove the entire backup API, not just local execution.
    debug!("Registering BackupPlugin");
    let backup_plugin = Box::new(BackupPlugin::new());
    plugin_manager.register_plugin(backup_plugin);

    // 11a. RevenuePlugin - per-project revenue tracking via inbound webhooks
    // (depends on database + encryption service only — no outbound API calls)
    debug!("Registering RevenuePlugin");
    let revenue_plugin = Box::new(RevenuePlugin::new());
    plugin_manager.register_plugin(revenue_plugin);

    // 11b. ObservabilityPlugin - unified Observe page (merges runtime logs,
    // requests, spans, errors, revenue into one event stream). Read-only,
    // no outbound calls; depends on the database only.
    debug!("Registering ObservabilityPlugin");
    let observability_plugin = Box::new(ObservabilityPlugin::new());
    plugin_manager.register_plugin(observability_plugin);

    // AI Chat Plugin - persistent AI debugging conversations (ADR-023). After the
    // AI gateway so the AiService it provides is registered.
    debug!("Registering AiChatPlugin");
    let ai_chat_plugin = Box::new(temps_ai_chat::AiChatPlugin::new());
    plugin_manager.register_plugin(ai_chat_plugin);

    // 12. ApiKeyPlugin - provides API key management (depends on auth services)
    debug!("Registering ApiKeyPlugin");
    let apikey_plugin = Box::new(ApiKeyPlugin::new());
    plugin_manager.register_plugin(apikey_plugin);

    // 13. ProxyPlugin - provides proxy services (depends on auth services)
    debug!("Registering ProxyPlugin");
    let proxy_plugin = Box::new(ProxyPlugin::new());
    plugin_manager.register_plugin(proxy_plugin);

    // 14. StaticFilesPlugin - provides static file serving (depends on config)
    debug!("Registering StaticFilesPlugin");
    let static_files_plugin = Box::new(StaticFilesPlugin::new());
    plugin_manager.register_plugin(static_files_plugin);

    // 15. ExternalPluginsPlugin - discovers and manages standalone binary plugins
    debug!("Registering ExternalPluginsPlugin");
    let external_plugin_config = temps_external_plugins::manager::ExternalPluginConfig::new(
        config.data_dir.clone(),
        config.database_url.clone(),
    )
    // Where this instance answers HTTP. A plugin's routes are only reachable
    // through the proxy, so a plugin that has to hand out a URL to something
    // outside the request (a sandboxed agent, a webhook receiver) cannot
    // construct one without being told the address the proxy listens on.
    .with_proxy_address(&config.address)
    .with_registry(external_plugin_registry);
    let external_plugins_plugin = Box::new(temps_external_plugins::ExternalPluginsPlugin::new(
        external_plugin_config,
    ));
    plugin_manager.register_plugin(external_plugins_plugin);

    // Extra plugins from the calling binary (EE, etc.). Registered last so
    // they can resolve every OSS service via `require_service`. See ADR 0001
    // §"Extension points exposed by OSS" — this is the
    // single seam between an OSS build and an EE-bundled binary.
    let extra_count = extra_plugins.len();
    for plugin in extra_plugins {
        debug!("Registering extra plugin: {}", plugin.name());
        plugin_manager.register_plugin(plugin);
    }
    if extra_count > 0 {
        info!(
            "Registered {} extra plugin(s) from binary entrypoint",
            extra_count
        );
    }

    // One line naming everything this profile left out, so an operator
    // wondering why an endpoint 404s does not have to read the source. The
    // capabilities endpoint answers the same question programmatically.
    if skipped_plugins.is_empty() {
        info!(profile = profile.as_str(), "All plugins registered");
    } else {
        info!(
            profile = profile.as_str(),
            skipped = %skipped_plugins.join(", "),
            docker_available,
            "Serve profile runs no local workloads: the listed plugins were not constructed and \
             their background tasks were not started. Applications run on worker nodes joined \
             with `temps join`; see GET /api/platform/features"
        );
    }

    // Initialize all plugins
    debug!("Initializing plugins");
    if let Err(e) = plugin_manager.initialize_plugins().await {
        let error_msg = format!("{}", e);
        tracing::error!("❌ Plugin initialization FAILED");
        tracing::error!("Error: {}", error_msg);
        tracing::error!("Error details: {:?}", e);
        tracing::error!("");
        tracing::error!("Most common causes:");
        tracing::error!("  • Missing GeoLite2-City.mmdb file");
        tracing::error!("  • Database connection failed");
        tracing::error!("  • Service initialization error");
        tracing::error!("");
        tracing::error!("Check the error message above for details.");
        return Err(anyhow::anyhow!(
            "Plugin initialization failed: {}",
            error_msg
        ));
    }
    debug!("All plugins initialized successfully");

    // `project_ip_gate_slot` (see the security guardrail comment on
    // `ConsoleApiParams::project_ip_gate_slot`) is fully resolved at this
    // exact point: `ProxyPlugin::initialize` -- which claims the slot from
    // whatever `Arc<dyn ProjectIpGate>` an EE plugin registered in phase 1,
    // if any did -- already ran as part of `initialize_plugins()` above
    // (two-phase: ALL plugins register, THEN ALL plugins initialize). There
    // is nothing left between here and the end of this function that could
    // change that outcome, so signal it now rather than after routers,
    // middleware, the admin gate, and the TCP listener are also built.
    //
    // `commands/serve/mod.rs` holds the proxy's startup on this signal
    // specifically to close a P1 security race (PR #725): before this fix,
    // the wait was tied to the FULL console being ready (whatever that
    // happened to take), so every IP-restricted project was reachable by
    // any client for however long console startup took, on every boot.
    // Firing here means the wait now resolves as soon as the one thing it
    // actually depends on is done, deterministically, in the overwhelming
    // majority of cases -- the bounded timeout on the other end becomes a
    // backstop against a genuinely hung plugin `initialize()`, not the
    // expected path.
    if let Some(signal) = ready_signal {
        let _ = signal.send(());
        debug!("Project IP gate slot resolved; signaled ready_signal early");
    }

    // Check if any users exist, if not prompt for admin email
    let service_context = plugin_manager.service_context();

    // Emit the anonymous `instance_started` telemetry event now that the
    // service registry is populated. Entirely best-effort: a missing reporter,
    // a failed count query, or a dead endpoint must never affect startup.
    if let Some(reporter) =
        service_context.get_service::<dyn temps_core::telemetry::TelemetryReporter>()
    {
        if reporter.is_enabled() {
            report_instance_started(reporter.as_ref(), db.as_ref()).await;
            // Keep "active instances" honest: a daily heartbeat so a live-but-idle
            // instance still checks in even when it isn't deploying. No-op when
            // telemetry is disabled (guarded above + report() no-ops anyway).
            spawn_heartbeat_task(reporter.clone(), db.clone());
            // Periodic aggregated error_summary flush (ERROR logs / console
            // 5xx / panics — counts only, never messages). Only spawned when
            // telemetry is enabled; the counters themselves are just bounded
            // in-process memory either way.
            spawn_error_summary_task(reporter.clone());
        }
    }
    if let Some(user_service) = service_context.get_service::<temps_auth::UserService>() {
        // Always ensure the system user (id=0) exists — needed for webhook-created
        // resources (e.g., GitHub App installations) that reference user_id=0
        ensure_system_user(db.as_ref()).await?;

        let users = user_service
            .get_all_users(false) // Don't include deleted users
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get users: {}", e))?;

        if users.is_empty() {
            debug!("No users found, prompting for admin email");

            // Initialize roles first to ensure they exist
            user_service
                .initialize_roles()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to initialize roles: {}", e))?;
            debug!("Initialized user roles");

            let configured_email = optional_environment_variable("TEMPS_ADMIN_EMAIL")?;
            let configured_password_file =
                optional_environment_variable("TEMPS_ADMIN_PASSWORD_FILE")?;
            if let Some((admin_email, admin_password)) = configured_initial_admin(
                configured_email.as_deref(),
                configured_password_file.as_deref(),
            )? {
                info!("Creating initial admin from configured email and password secret file");
                create_initial_admin_user(db.as_ref(), &admin_email, Some(&admin_password)).await?;
            } else if let Some(admin_email) = prompt_for_admin_email()? {
                create_initial_admin_user(db.as_ref(), &admin_email, None).await?;
            } else {
                return Err(anyhow::anyhow!("Valid admin email is required to continue"));
            }
        }
    } else {
        debug!("UserService not available, skipping user initialization");
    }

    // Unattended Temps Cloud enrollment via TEMPS_CLOUD_ENROLLMENT_CODE (see
    // the doc comment on `TEMPS_CLOUD_ENROLLMENT_CODE_VAR` above; ADR 0040 in
    // the Cloud repo). Deliberately not nested inside the `users.is_empty()`
    // block above: idempotency comes from the already-linked check inside
    // `bootstrap_cloud_enrollment_from_env`, not from whether this was the
    // instance's very first boot, so a restart with a leftover env var is
    // always safe. Best-effort and off the startup critical path: this spawns
    // the attempt and returns immediately, so listener binding below never
    // waits on the Cloud backend. The handle is joined (bounded) by the
    // graceful-shutdown future further down.
    let enrollment_bootstrap = bootstrap_cloud_enrollment_from_env(service_context);

    // NOTE: The backup scheduler is started by `BackupPlugin` during plugin
    // initialization (see `temps-backup/src/plugin.rs`). Do NOT start it here as
    // well -- spawning a second scheduler loop makes both loops independently
    // find each due `backup_schedules` row and enqueue a `Job::BackupRequested`,
    // producing two completed backup runs per service at the same timestamp.

    // Start certificate renewal scheduler (optional - fails gracefully if TlsService unavailable)
    if let Some(tls_service) = service_context.get_service::<temps_domains::TlsService>() {
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let scheduler_token = cancellation_token.clone();
        let scheduler_service = tls_service.clone();

        tokio::spawn(async move {
            debug!("Starting certificate renewal scheduler");
            // Catch any panics to prevent scheduler issues from crashing the main task
            let result = std::panic::AssertUnwindSafe(async {
                scheduler_service
                    .start_certificate_renewal_scheduler(scheduler_token)
                    .await
            })
            .catch_unwind()
            .await;

            match result {
                Ok(Ok(())) => {
                    debug!("Certificate renewal scheduler completed normally");
                }
                Ok(Err(e)) => {
                    tracing::warn!("Certificate renewal scheduler error (non-fatal): {}", e);
                }
                Err(_) => {
                    tracing::warn!(
                        "Certificate renewal scheduler panicked (non-fatal) - scheduler stopped"
                    );
                }
            }
        });

        debug!("Certificate renewal scheduler started in background");
    } else {
        tracing::warn!(
            "TlsService not available - certificate renewal scheduler disabled. \
             This is non-fatal but automatic certificate renewal will not work."
        );
    }

    // Start disk space monitoring if ConfigService and AlarmService are available
    if let (Some(config_service), Some(alarm_service)) = (
        service_context.get_service::<temps_config::ConfigService>(),
        service_context.get_service::<AlarmService>(),
    ) {
        let monitor = Arc::new(DiskSpaceMonitor::new(config_service.clone(), alarm_service));

        tokio::spawn(async move {
            monitor.start_monitoring().await;
        });

        debug!("Disk space monitoring started in background");
    } else {
        tracing::warn!(
            "ConfigService or AlarmService not available - disk space monitoring disabled."
        );
    }

    // ADR-041 §2a: wire the alarm service into the Traefik discovery watcher so
    // certificate-drift events are visible in the alarm panel, not just in logs.
    // This must run after MonitoringPlugin (step 9.8) has registered AlarmService.
    // We bridge via `DriftAlarmSink` to avoid a dependency cycle
    // (temps-monitoring depends on temps-deployer).
    if let Some(alarm_service) = service_context.get_service::<AlarmService>() {
        struct AlarmServiceDriftSink(Arc<AlarmService>);
        #[async_trait::async_trait]
        impl DriftAlarmSink for AlarmServiceDriftSink {
            async fn notify_container_drift(
                &self,
                host: String,
                authorized_container: String,
                current_container: String,
            ) {
                use temps_monitoring::alarm_service::{AlarmSeverity, AlarmType, FireAlarmRequest};
                let detail = format!(
                    "Host '{host}' is now served by container '{current}' but TLS was \
                     authorized for container '{authorized}'. The certificate is still \
                     valid but may be delivered to the wrong container if the new one is \
                     not legitimate. Deauthorize and re-authorize once the correct \
                     container is confirmed.",
                    host = host,
                    current = current_container,
                    authorized = authorized_container,
                );
                let metadata = serde_json::json!({
                    "host": host,
                    "authorized_container": authorized_container,
                    "current_container": current_container,
                });
                let request = FireAlarmRequest {
                    project_id: None,
                    environment_id: None,
                    deployment_id: None,
                    container_id: None,
                    service_id: None,
                    alarm_type: AlarmType::TraefikContainerDrift,
                    severity: AlarmSeverity::Critical,
                    title: format!("Certificate drift: {host}"),
                    message: detail,
                    metadata: Some(metadata),
                };
                if let Err(e) = self.0.fire_alarm(request).await {
                    tracing::error!(
                        host = %host,
                        error = %e,
                        "Failed to fire TraefikContainerDrift alarm; drift is still \
                         recorded in the database"
                    );
                }
            }
        }
        traefik_discovery.inject_alarm_sink(Arc::new(AlarmServiceDriftSink(alarm_service)));
    }

    // ADR-041 §8: wire the TLS provisioner for Traefik label discovery.
    //
    // `temps-deployments` cannot depend on `temps-domains` directly (that would
    // introduce a dependency cycle), so the trait `DiscoveredHostTlsProvisioner`
    // is declared in `temps-deployments` and implemented here — in the serve
    // wiring layer that already depends on both crates — following the same
    // adapter pattern used above for `AlarmServiceDriftSink`.
    //
    // Both DomainService and CertificateRepository are registered by
    // DomainsPlugin::register_services, which has already run by this point
    // (all plugins run register_services before initialize_plugins returns).
    // If DomainsPlugin is absent the require_service below will panic at
    // startup with a clear error, which is the correct behaviour for a missing
    // required dependency (CLAUDE.md: "Use `Arc<T>` and fail at startup if missing").
    {
        use temps_deployments::services::traefik_discovery_service::{
            DiscoveredHostTlsProvisioner, TlsProvisionerError,
        };
        use temps_domains::tls::models::{Certificate, CertificateStatus};
        use temps_domains::tls::repository::CertificateRepository;
        use temps_domains::DomainService;

        struct TraefikTlsProvisioner {
            domain_service: Arc<DomainService>,
            cert_repo: Arc<dyn CertificateRepository>,
            config_service: Arc<temps_config::ConfigService>,
            dns_provider_service: Arc<temps_dns::services::DnsProviderService>,
        }

        impl TraefikTlsProvisioner {
            /// Read `letsencrypt.email` from settings — mirrors TlsService::get_acme_email.
            async fn get_acme_email(&self) -> String {
                if let Ok(settings) = self.config_service.get_settings().await {
                    if let Some(email) = settings.letsencrypt.email {
                        let email = email.trim().to_string();
                        if !email.is_empty() {
                            return email;
                        }
                    }
                }
                String::new()
            }
        }

        #[async_trait::async_trait]
        impl DiscoveredHostTlsProvisioner for TraefikTlsProvisioner {
            async fn request_acme_cert(
                &self,
                host: &str,
                challenge_type: &str,
            ) -> Result<(), TlsProvisionerError> {
                let email = self.get_acme_email().await;

                // Check whether a domains row already exists for this host.
                let existing = self.cert_repo.find_certificate(host).await.map_err(|e| {
                    TlsProvisionerError::Failed {
                        host: host.to_string(),
                        reason: e.to_string(),
                    }
                })?;

                match existing {
                    None => {
                        // No row yet — create one with the declared challenge
                        // type. If the ACME challenge request below fails, this
                        // row is deliberately left in place rather than rolled
                        // back: `TraefikDiscoveryAdminService::authorize_acme_cert`
                        // already wrote a `traefik_route_certificates` claim for
                        // this host before calling here, so a retry is never
                        // blocked by the host-ownership check, and it reuses
                        // this same pending row via the branch below.
                        self.domain_service
                            .create_domain(host, challenge_type)
                            .await
                            .map_err(|e| TlsProvisionerError::Failed {
                                host: host.to_string(),
                                reason: e.to_string(),
                            })?;
                    }
                    Some(cert) if cert.verification_method == challenge_type => {
                        // Row exists with a matching method — reuse it as-is.
                    }
                    Some(cert) => {
                        // Row exists but the stored method differs — 409.
                        return Err(TlsProvisionerError::VerificationMethodConflict {
                            host: host.to_string(),
                            stored: cert.verification_method,
                            declared: challenge_type.to_string(),
                        });
                    }
                };

                self.domain_service
                    .request_challenge(host, &email)
                    .await
                    .map_err(|e| TlsProvisionerError::Failed {
                        host: host.to_string(),
                        reason: e.to_string(),
                    })?;

                Ok(())
            }

            async fn save_imported_cert(
                &self,
                host: &str,
                certificate_pem: &str,
                key_pem: &str,
                renewal_method: &str,
                not_after: chrono::DateTime<chrono::Utc>,
            ) -> Result<i32, TlsProvisionerError> {
                let cert = Certificate {
                    id: 0,
                    domain: host.to_string(),
                    certificate_pem: certificate_pem.to_string(),
                    private_key_pem: key_pem.to_string(),
                    expiration_time: not_after,
                    last_renewed: Some(chrono::Utc::now()),
                    is_wildcard: false,
                    verification_method: renewal_method.to_string(),
                    status: CertificateStatus::Active,
                };
                let saved = self.cert_repo.save_certificate(cert).await.map_err(|e| {
                    TlsProvisionerError::Failed {
                        host: host.to_string(),
                        reason: e.to_string(),
                    }
                })?;
                Ok(saved.id)
            }

            async fn dns_zone_is_auto_managed(
                &self,
                host: &str,
            ) -> Result<bool, TlsProvisionerError> {
                self.dns_provider_service
                    .find_provider_for_domain(host)
                    .await
                    .map(|found| found.is_some())
                    .map_err(|e| TlsProvisionerError::Failed {
                        host: host.to_string(),
                        reason: e.to_string(),
                    })
            }
        }

        let domain_service = service_context.require_service::<DomainService>();
        let cert_repo = service_context.require_service::<dyn CertificateRepository>();
        let config_service_for_provisioner =
            service_context.require_service::<temps_config::ConfigService>();
        let dns_provider_service_for_provisioner =
            service_context.require_service::<temps_dns::services::DnsProviderService>();
        let provisioner: Arc<dyn DiscoveredHostTlsProvisioner> = Arc::new(TraefikTlsProvisioner {
            domain_service,
            cert_repo,
            config_service: config_service_for_provisioner,
            dns_provider_service: dns_provider_service_for_provisioner,
        });
        service_context.register_service(provisioner);
        debug!("Traefik TLS provisioner (ADR-041 §8) registered");
    }

    // Start alarm service, outage detection, and container health monitoring.
    // AlarmService is already constructed and registered by MonitoringPlugin
    // (step 9.8 above). We retrieve the same Arc here so the background loops
    // share the single instance with the HTTP handlers.
    if let (Some(notification_service), Some(queue_service), Some(alarm_service)) = (
        service_context.get_service::<dyn temps_core::notifications::NotificationService>(),
        service_context.get_service::<dyn temps_core::JobQueue>(),
        service_context.get_service::<AlarmService>(),
    ) {
        // alarm_service is the same Arc<AlarmService> registered by MonitoringPlugin.

        // Start event-driven outage detection (listens to StatusCheckCompleted jobs).
        // The job queue is attached so monitoring.downtime workflows can be fired
        // automatically when an outage is detected.
        let outage_service = Arc::new(
            OutageDetectionService::new(db.clone(), notification_service, alarm_service.clone())
                .with_job_queue(queue_service.clone()),
        );

        let job_receiver = queue_service.subscribe();
        tokio::spawn(async move {
            outage_service.start_monitoring(job_receiver).await;
        });

        debug!("Event-driven outage detection service started (listening to StatusCheckCompleted jobs)");

        // Start container health monitoring (restart count, resource usage)
        if let Some(container_deployer) =
            service_context.get_service::<dyn temps_deployer::ContainerDeployer>()
        {
            // Build the metrics store if monitoring is enabled, so container
            // resource metrics are written alongside the alarm logic.
            let container_metrics_store: Option<Arc<dyn temps_metrics::MetricsStore>> = {
                use temps_core::MetricsStoreKind;
                use temps_metrics::{MetricsStore, TimescaleMetricsStore};

                // Provide a metrics store for container resource metrics. When
                // the monitoring store is ClickHouse and CH is configured, use
                // it; otherwise (TimescaleDb, or CH selected but unconfigured)
                // fall back to TimescaleDB.
                match service_context.get_service::<temps_config::ConfigService>() {
                    Some(cfg_svc) => match cfg_svc.get_settings().await {
                        Ok(settings) => match settings.monitoring.store {
                            MetricsStoreKind::TimescaleDb => {
                                Some(Arc::new(TimescaleMetricsStore::new(db.clone()))
                                    as Arc<dyn MetricsStore>)
                            }
                            MetricsStoreKind::ClickHouse => build_ch_metrics_store(&config)
                                .or_else(|| {
                                    Some(Arc::new(TimescaleMetricsStore::new(db.clone()))
                                        as Arc<dyn MetricsStore>)
                                }),
                        },
                        _ => None,
                    },
                    None => None,
                }
            };

            let mut health_monitor = ContainerHealthMonitor::new(
                db.clone(),
                container_deployer,
                alarm_service.clone(),
                ContainerHealthConfig::default(),
            );

            if let Some(ms) = container_metrics_store {
                health_monitor = health_monitor.with_metrics_store(ms);
            }

            let health_monitor = Arc::new(health_monitor);
            tokio::spawn(async move {
                health_monitor.start().await;
            });

            debug!("Container health monitor started (poll interval: 30s)");
        } else {
            debug!("ContainerDeployer not available - container health monitoring disabled");
        }

        // Start MetricsScraper for external service DB-level metrics
        // (postgres, redis, mongodb) when monitoring is enabled.
        if let (Some(cfg_svc), Some(enc_svc)) = (
            service_context.get_service::<temps_config::ConfigService>(),
            service_context.get_service::<temps_core::EncryptionService>(),
        ) {
            use temps_core::MetricsStoreKind;
            use temps_metrics::{MetricsScraper, MetricsStore, TimescaleMetricsStore};

            // The metrics store and scraper are ALWAYS wired up — the per-service
            // `metrics_enabled` flag is the single source of truth for what gets
            // scraped. The scraper idles (near-zero cost) when no service has
            // monitoring enabled, so a user clicking "Enable Monitoring" on a
            // service just works without an operator first flipping a global flag.
            //
            // When the monitoring store is ClickHouse AND TEMPS_CLICKHOUSE_* is
            // configured, this single binding becomes the ClickHouse store used
            // by the HTTP query endpoints, the AlertEvaluator, AND the scraper's
            // writes. Otherwise (TimescaleDb, or CH selected but unconfigured)
            // it falls back to TimescaleDB unchanged.
            match cfg_svc.get_settings().await {
                Ok(settings) => {
                    let metrics_store: Arc<dyn MetricsStore> = match settings.monitoring.store {
                        MetricsStoreKind::ClickHouse => build_ch_metrics_store(&config)
                            .unwrap_or_else(|| Arc::new(TimescaleMetricsStore::new(db.clone()))),
                        MetricsStoreKind::TimescaleDb => {
                            Arc::new(TimescaleMetricsStore::new(db.clone()))
                        }
                    };

                    // Register the metrics store so plugins (e.g. providers)
                    // can retrieve it for HTTP query endpoints.
                    service_context.register_service(metrics_store.clone());

                    let scraper = Arc::new(MetricsScraper::new(
                        db.clone(),
                        metrics_store.clone(),
                        cfg_svc,
                        enc_svc,
                    ));

                    tokio::spawn(async move {
                        scraper.start().await;
                    });

                    debug!(
                        "MetricsScraper started (scrapes only services with metrics_enabled=true)"
                    );

                    // Start AlertEvaluator alongside the scraper.
                    // It reads monitoring_alert_rules from the DB and evaluates
                    // each rule against the most-recent value from MetricsStore,
                    // firing/resolving alarms via the shared AlarmService.
                    let evaluator = Arc::new(temps_monitoring::AlertEvaluator::new(
                        db.clone(),
                        metrics_store,
                        alarm_service.clone(),
                    ));

                    tokio::spawn(async move {
                        evaluator.start().await;
                    });

                    debug!("AlertEvaluator started (metric threshold alerts, 30s interval)");

                    // Start hourly pruning job for raw service_metrics rows.
                    // Continuous aggregates (hourly/daily rollups) have their
                    // own TimescaleDB retention policies; this only handles
                    // the raw hypertable rows.
                    {
                        use chrono::{Duration, Utc};
                        use temps_core::MetricsStoreKind;
                        use temps_metrics::{MetricsStore, TimescaleMetricsStore};

                        let prune_db = db.clone();
                        let prune_cfg =
                            service_context.get_service::<temps_config::ConfigService>();
                        // Clone the server config so the ClickHouse arm can build
                        // a store inside the spawned loop without re-spawning the
                        // migration task each tick (CH prune is a TTL-backed no-op).
                        let prune_config = config.clone();

                        tokio::spawn(async move {
                            let mut interval =
                                tokio::time::interval(std::time::Duration::from_secs(3600));
                            loop {
                                interval.tick().await;
                                let Some(ref cfg_svc) = prune_cfg else {
                                    break;
                                };
                                let settings = match cfg_svc.get_settings().await {
                                    Ok(s) => s,
                                    Err(e) => {
                                        tracing::warn!(
                                            "PruneMetrics: failed to read settings: {e}"
                                        );
                                        continue;
                                    }
                                };
                                // When ClickHouse is the active metrics store, the
                                // table's native TTL enforces retention — there is
                                // nothing for prune() to do. Skip the tick entirely
                                // rather than build a CH store just to call a no-op.
                                if matches!(settings.monitoring.store, MetricsStoreKind::ClickHouse)
                                    && prune_config.is_clickhouse_enabled()
                                {
                                    continue;
                                }
                                let store: Arc<dyn MetricsStore> = match settings.monitoring.store {
                                    MetricsStoreKind::TimescaleDb => {
                                        Arc::new(TimescaleMetricsStore::new(prune_db.clone()))
                                    }
                                    MetricsStoreKind::ClickHouse => {
                                        // CH selected but unconfigured — match the
                                        // read/write path's TimescaleDB fallback.
                                        // (The CH-configured case is skipped above.)
                                        Arc::new(TimescaleMetricsStore::new(prune_db.clone()))
                                    }
                                };
                                let cutoff = Utc::now()
                                    - Duration::days(settings.monitoring.retention_raw_days as i64);
                                match store.prune(cutoff).await {
                                    Ok(n) => {
                                        debug!(
                                            "PruneMetrics: pruned {} raw metric rows older than {}",
                                            n, cutoff
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!("PruneMetrics: prune failed: {e}");
                                    }
                                }
                            }
                        });

                        debug!("Metrics pruning job scheduled (hourly)");
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to read monitoring settings: {e} — MetricsScraper and AlertEvaluator not started");
                }
            }
        } else {
            debug!("ConfigService or EncryptionService not available — MetricsScraper and AlertEvaluator not started");
        }
    } else {
        tracing::warn!(
            "NotificationService or JobQueue not available - outage detection and alarm service disabled."
        );
    }

    // Start external service health monitoring (Postgres/Redis/MongoDB/RustFS TCP probes)
    if let (Some(alarm_service), Some(external_service_manager)) = (
        service_context.get_service::<AlarmService>(),
        service_context.get_service::<temps_providers::ExternalServiceManager>(),
    ) {
        use temps_providers::health_monitor::{
            ExternalServiceHealthConfig, ExternalServiceHealthMonitor,
        };
        let mut health_monitor = ExternalServiceHealthMonitor::new(
            db.clone(),
            external_service_manager,
            alarm_service,
            ExternalServiceHealthConfig::default(),
            docker_handle.clone(),
            service_context.require_service::<temps_core::EncryptionService>(),
        );

        // Attach the shared metrics store (registered by the MetricsScraper
        // block above) so the monitor records container CPU/memory history
        // for services with metrics enabled.
        if let Some(metrics_store) =
            service_context.get_service::<dyn temps_metrics::MetricsStore>()
        {
            health_monitor = health_monitor.with_metrics_store(metrics_store);
        }

        let health_monitor = Arc::new(health_monitor);

        // Register so the providers plugin can pick it up and expose a
        // manual-trigger endpoint that reuses the monitor's check logic.
        service_context.register_service(health_monitor.clone());

        let loop_handle = health_monitor.clone();
        tokio::spawn(async move {
            loop_handle.start().await;
        });

        debug!("External service health monitor started (poll interval: 30s)");
    } else {
        tracing::warn!(
            "AlarmService or ExternalServiceManager not available - external service health monitoring disabled."
        );
    }

    // OTel background tasks: anomaly detection and health computation require
    // iterating over active project IDs, which will be wired up when project
    // discovery is integrated. The rate limiter is self-cleaning (evicts on check).
    if service_context
        .get_service::<temps_otel::OtelService>()
        .is_some()
    {
        debug!("OTel plugin registered successfully; background tasks pending project discovery integration");
    }

    // Multi-node: create NodeService, register node routes, and start health check
    let config_service_for_nodes = service_context.require_service::<temps_config::ConfigService>();
    let node_service = Arc::new(NodeService::new(db.clone()));
    let encryption_service_for_nodes =
        service_context.require_service::<temps_core::EncryptionService>();
    let node_telemetry = service_context
        .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
        .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));
    let node_app_state = Arc::new(NodeAppState {
        node_service: node_service.clone(),
        db: db.clone(),
        config_service: config_service_for_nodes,
        encryption_service: encryption_service_for_nodes,
        telemetry: node_telemetry,
        rate_limiter: Arc::new(temps_deployments::handlers::nodes::RegistrationRateLimiter::new()),
        enrollment_token_service: Arc::new(temps_config::EnrollmentTokenService::new(db.clone())),
        alarm_service: service_context.get_service::<AlarmService>(),
        audit_service: service_context.require_service::<dyn temps_core::AuditLogger>(),
    });
    let node_routes =
        temps_deployments::handlers::nodes::configure_routes().with_state(node_app_state);

    // Start periodic node health check with failover (every 60s)
    {
        let health_node_service = node_service.clone();
        let health_db = db.clone();
        let deployment_service_for_failover =
            service_context.get_service::<temps_deployments::DeploymentService>();
        let health_alarm_service = service_context.get_service::<AlarmService>();
        let health_config_service = service_context.get_service::<temps_config::ConfigService>();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                // Sample the control plane's own host metrics so the synthetic
                // control-plane node shows live CPU/mem/disk (it has no agent
                // heartbeat). Always runs, independent of alert config.
                refresh_control_plane_metrics();
                let offline_ids = check_node_health(&health_node_service, health_db.as_ref()).await;
                if !offline_ids.is_empty() {
                    tracing::info!(
                        "Node health check: marked {} node(s) as offline",
                        offline_ids.len()
                    );
                    // Alert operators that worker node(s) went down (best-effort).
                    if let Some(ref alarm_service) = health_alarm_service {
                        notify_nodes_offline(&offline_ids, &health_node_service, alarm_service)
                            .await;
                    }
                    // Trigger failover redeployment for affected environments
                    if let Some(ref deployment_service) = deployment_service_for_failover {
                        failover_offline_nodes(
                            &offline_ids,
                            &health_node_service,
                            deployment_service,
                        )
                        .await;
                    }
                }

                // Alert on node resource pressure (CPU/mem/disk) against the
                // operator-configurable thresholds in settings.multi_node.
                if let (Some(ref alarm_service), Some(ref config_service)) =
                    (&health_alarm_service, &health_config_service)
                {
                    check_node_resources(health_db.as_ref(), config_service, alarm_service).await;
                    // The control plane isn't a `nodes` row, so it's excluded
                    // from the query above — alert on its own metrics separately.
                    check_control_plane_resources(config_service, alarm_service).await;
                }

                // Transition fully-drained nodes from "draining" to "drained".
                // Without this, a node whose containers have all migrated stays
                // stuck in "draining" forever (it never auto-completes), so the
                // operator can never safely remove it. (ADR-020 WS-5.1 / lifecycle-7)
                let drained_ids = check_drain_completion(&health_node_service).await;
                if !drained_ids.is_empty() {
                    tracing::info!(
                        "Node drain check: {} node(s) completed draining",
                        drained_ids.len()
                    );
                }
            }
        });
        debug!("Node health check scheduler with failover started (every 60s)");
    }

    // Build the application with all plugin routes and OpenAPI schemas
    debug!("Building application with plugin routes");

    // Internal route-sync endpoint for the worker-side internal edge
    // proxy (Option 1 in the route-sync design). Workers long-poll
    // here to mirror the CP's `*.temps.local` route table without
    // needing direct DB access.
    let route_sync_state = Arc::new(temps_routes::route_sync::RouteSyncAppState {
        db: db.clone(),
        peer_table: route_table.clone(),
    });
    let route_sync_routes =
        temps_routes::route_sync::configure_routes().with_state(route_sync_state);

    // Build the split application: public routes (event ingest, AI gateway,
    // session replay ingest, etc.) and admin routes (auth, dashboard, CRUD).
    let split = plugin_manager
        .build_split_application()
        .map_err(|e| anyhow::anyhow!("Failed to build application: {}", e))?;

    // ADR-024: Populate the ApiToolsHandle now that the Axum router is fully
    // assembled.  InternalApiCaller holds a clone of the merged admin router
    // (which carries the auth middleware layers) and the unified OpenAPI doc
    // — both are only available at this point, after plugin initialisation and
    // build_split_application().  The handle was registered as a service by
    // AiChatPlugin::register_services(); all adapters (OSS ChatTool, EE rig
    // Tool) retrieved it at that time and hold clones that share the same
    // OnceLock.
    {
        let service_context = plugin_manager.service_context();
        if let Some(handle) = service_context.get_service::<temps_ai_api_tools::ApiToolsHandle>() {
            match create_openapi(&plugin_manager) {
                Ok(openapi) => {
                    // The admin router carries the full plugin API surface plus
                    // the auth middleware stack (permission_guard! reads
                    // AuthContext from extensions, which the middleware injects).
                    // We clone it here so InternalApiCaller can replay synthetic
                    // requests through it without consuming the original.
                    //
                    // ALLOWLIST for the AI `call_api` tool — see
                    // `ai_read_allowlist()` below for the full curated list and
                    // the security rationale behind what is/isn't included.
                    let allowlist: Vec<String> = ai_read_allowlist();
                    // Plus the narrow, separately-vetted set of read-only POSTs
                    // — see `ai_read_safe_posts()` for the rule governing it.
                    let safe_posts: Vec<String> = ai_read_safe_posts();
                    let caller =
                        temps_ai_api_tools::InternalApiCaller::new_allowlisted_with_safe_posts(
                            split.admin.clone(),
                            &openapi,
                            allowlist.clone(),
                            safe_posts.clone(),
                        );
                    // Diagnostic: report which allowlist entries actually
                    // resolved to a real operation in the OpenAPI doc, and
                    // loudly flag any that did not (a typo or a wrong
                    // method/operation_id silently drops the op otherwise) —
                    // mirrors the same check already done for the write
                    // allowlist below.
                    let resolved = caller.indexed_operation_ids();
                    let unresolved: Vec<&String> = allowlist
                        .iter()
                        .chain(safe_posts.iter())
                        .filter(|id| !resolved.contains(id))
                        .collect();
                    info!(
                        resolved_count = resolved.len(),
                        allowlist_count = allowlist.len() + safe_posts.len(),
                        "AI read tool: indexed read operations"
                    );
                    if !unresolved.is_empty() {
                        tracing::warn!(
                            ?unresolved,
                            "AI read tool: allowlisted read operations did NOT resolve to \
                             an OpenAPI operation and are unavailable — check the operation_id"
                        );
                    }
                    handle.set(caller);
                    debug!("ADR-024: InternalApiCaller populated in ApiToolsHandle");

                    // ── Approval-aware WRITE tool ──
                    // Populate the separate WriteApiToolsHandle with a method-aware
                    // caller over a CURATED allowlist of mutating operations.
                    // `temps_write` applies the active harness approval mode and
                    // replays authorized mutations through this same router
                    // (permission_guard! + audit). This allowlist is conservative
                    // by design: high-value, mostly-reversible
                    // lifecycle + config operations. Adding an entry is a product +
                    // security decision (what may the AI propose for a human to run).
                    if let Some(write_handle) =
                        service_context.get_service::<temps_ai_api_tools::WriteApiToolsHandle>()
                    {
                        let write_allowlist: Vec<String> = ai_write_allowlist();
                        let write_caller =
                            temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
                                split.admin.clone(),
                                &openapi,
                                write_allowlist.clone(),
                            );
                        // Diagnostic: report which allowlist entries actually
                        // resolved to a real operation in the OpenAPI doc, and
                        // loudly flag any that did not (a typo or a wrong
                        // method/operation_id silently drops the op otherwise).
                        let resolved = write_caller.indexed_operation_ids();
                        let unresolved: Vec<&String> = write_allowlist
                            .iter()
                            .filter(|id| !resolved.contains(id))
                            .collect();
                        info!(
                            resolved_count = resolved.len(),
                            allowlist_count = write_allowlist.len(),
                            resolved = ?resolved,
                            "AI write tool: indexed write operations"
                        );
                        if !unresolved.is_empty() {
                            tracing::warn!(
                                ?unresolved,
                                "AI write tool: allowlisted write operations did NOT resolve to \
                                 an OpenAPI operation and are unavailable — check the operation_id"
                            );
                        }
                        write_handle.set(write_caller);
                        debug!("AI write tool: WriteApiToolsHandle populated (curated allowlist)");
                    } else {
                        debug!("AI write tool: WriteApiToolsHandle not registered; skipping");
                    }
                }
                Err(e) => {
                    // Non-fatal: the AI API tools simply won't be available this
                    // run.  Log the error so operators can diagnose it without
                    // taking down the whole server.
                    tracing::warn!(
                        error = %e,
                        "ADR-024: failed to build OpenAPI doc for InternalApiCaller; \
                         AI API tools will not be available"
                    );
                }
            }
        } else {
            // AiChatPlugin is not loaded (e.g. AI is disabled). This is expected
            // in reduced-feature deployments; no action needed.
            debug!("ADR-024: ApiToolsHandle not registered (AiChatPlugin absent); skipping InternalApiCaller setup");
        }
    }

    // Agent-facing node + route-sync routes are public (workers anywhere on
    // the internet POST to them with bearer tokens).
    let public_router = split.public.merge(node_routes).merge(route_sync_routes);

    // Use the caller-supplied admin-gate when present (so the proxy and the
    // console share one source of truth) and otherwise build a fresh one.
    let (admin_gate_service, admin_gate_handle) =
        match (provided_admin_gate_service, provided_admin_gate_handle) {
            (Some(svc), Some(handle)) => (svc, handle),
            _ => super::admin_gate_service::AdminGateService::new(
                db.clone(),
                &config.admin_allowed_ips,
                &config.admin_allowed_hosts,
                config.admin_trust_forwarded_for,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize admin gate: {}", e))?,
        };
    let admin_gate_state = Arc::new(super::admin_gate_handler::AdminGateAppState {
        service: admin_gate_service,
    });
    // Re-apply the plugin middleware stack (auth, request metadata, audit)
    // to our standalone admin-gate routes. Without this, RequireAuth finds
    // no AuthContext injected and the route 401s for logged-in users —
    // `Router::merge` does not propagate parent layers to merged routes.
    let admin_gate_routes = super::admin_gate_handler::configure_routes(admin_gate_state);
    let admin_gate_routes = plugin_manager
        .apply_middleware_to_router(admin_gate_routes, plugin_manager.get_middleware());

    // Swagger UI + the embedded SPA only live on the admin surface. So do
    // the admin-gate management routes.
    let admin_router = split
        .admin
        .merge(create_swagger_router(&plugin_manager)?)
        .merge(admin_gate_routes);

    // Wrap each surface in /api like the original single-router did, except
    // for the SPA fallback which serves the dashboard at the document root.
    // Health probes (`/healthz`, `/readyz`) live at the document root on the
    // PUBLIC surface so they are reachable without auth and without the admin
    // gate — a supervisor or load balancer must be able to poll them even when
    // the admin IP allowlist is active. The public surface is also the one that
    // exists in every topology (single- and dual-listener), so probes work
    // regardless of `console_admin_address`.
    // The router plugins reach over the platform channel.
    //
    // Built from the same public + admin routes the console serves, but
    // deliberately without the SPA fallback (a plugin wants the API, not
    // index.html) and without the admin IP gate (that gate exists to keep
    // browsers off the admin listener from untrusted networks; a channel
    // call arrives in-process from a plugin the operator installed, and is
    // authorised by an actor token plus the handler's own permission check).
    let plugin_api_router =
        Router::new().nest("/api", public_router.clone().merge(admin_router.clone()));

    // ADR-045 §3: a second clone of `admin_router`, shaped exactly like
    // `admin_app` below (`nest("/api", ..)` + the static-file fallback) but
    // taken *before* that router is wrapped in the admin IP-allowlist gate —
    // for the same reason `plugin_api_router` above omits it: authorization
    // for a console-proxied request comes from Cloud's own auth plus the
    // browser's session cookie, not from network topology. Installed into
    // the shared `ConsoleDispatchSlot` near the `RouterHostApi` wiring below,
    // for `temps-cloud-client::console_proxy::ConsoleProxyWorker` (started
    // elsewhere, once the `cloud.console_access_enabled` setting exists) to
    // drive in-process.
    let console_router = Router::new()
        .nest("/api", admin_router.clone())
        .fallback(serve_static_file);

    // Build root-level MCP routes (ADR-039). These live outside /api so the
    // CLI wizard's unauthenticated probe (GET /mcp/tools) works without a key.
    // The authenticated sub-router gets the full plugin middleware stack (auth,
    // request metadata) applied so RequireAuth works just like any /api handler.
    let mcp_root_router = {
        let service_context = plugin_manager.service_context();
        if let Some(mcp_state) = service_context.get_service::<McpHandlerState>() {
            let mcp_routers = temps_mcp_server::build_mcp_routers(mcp_state);
            let auth_mcp = plugin_manager.apply_middleware_to_router(
                mcp_routers.authenticated,
                plugin_manager.get_middleware(),
            );
            Router::new().merge(mcp_routers.public).merge(auth_mcp)
        } else {
            debug!("McpHandlerState not registered; MCP routes skipped");
            Router::new()
        }
    };

    let public_app = Router::new()
        .merge(health_router(ready_flag.clone()))
        .merge(mcp_root_router)
        .nest("/api", public_router)
        .layer(axum::middleware::from_fn(track_server_errors));

    // Platform-console listener: when an embedding binary overrode the root
    // bundle AND configured an address, serve the ORIGINAL console (same
    // admin API + admin gate) on its own listener/origin.
    let platform_router =
        if WEBSITE_OVERRIDE.get().is_some() && PLATFORM_CONSOLE_ADDR.get().is_some() {
            Some(admin_router.clone())
        } else {
            None
        };

    let admin_app = Router::new()
        .nest("/api", admin_router)
        .fallback(serve_static_file)
        .layer(axum::middleware::from_fn(track_server_errors));

    // Defense-in-depth: the Pingora proxy is now the primary enforcer (it
    // 404s gated requests before they ever reach this listener). The axum
    // middleware below only matters when something connects to the console
    // listener directly — e.g. loopback debugging, or a deployment where
    // the operator points an external reverse-proxy at console_address
    // instead of going through Pingora. The middleware short-circuits when
    // the active config is a noop, so the perf cost is negligible.
    let admin_app = admin_app.layer(axum::middleware::from_fn_with_state(
        admin_gate_handle.clone(),
        super::admin_gate::admin_gate,
    ));

    if let (Some(router), Some(addr)) = (platform_router, PLATFORM_CONSOLE_ADDR.get()) {
        let platform_app = Router::new()
            .nest("/api", router)
            .fallback(serve_original_console)
            .layer(axum::middleware::from_fn(track_server_errors))
            .layer(axum::middleware::from_fn_with_state(
                admin_gate_handle.clone(),
                super::admin_gate::admin_gate,
            ));
        let listener = TcpListener::bind(addr).await?;
        info!("Platform console (original temps UI) listening on {addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                platform_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            {
                tracing::error!("Platform console listener failed: {e}");
            }
        });
    }

    info!("Plugin system initialized successfully with static file serving");

    // ADR-045 §3: install the just-assembled `console_router` into a shared
    // slot for the console-proxy dispatcher — the same "shared slot, filled
    // post-construction" shape `RouterHostApi`'s bridge uses just below, and
    // for the same reason: the router does not exist until this point in
    // startup. The Cloud plugin registers the slot (and starts
    // `ConsoleProxyWorker` reading from it) during service registration;
    // this fills it. The fallback only exists for builds without that
    // plugin, so the console still starts — nothing reads the slot then.
    let console_dispatch_slot = match plugin_manager
        .service_context()
        .get_service::<temps_cloud_client::ConsoleDispatchSlot>()
    {
        Some(slot) => slot.as_ref().clone(),
        None => {
            let slot = temps_cloud_client::ConsoleDispatchSlot::new();
            plugin_manager
                .service_context()
                .register_service(Arc::new(slot.clone()));
            slot
        }
    };
    console_dispatch_slot
        .set(Arc::new(temps_cloud_client::ConsoleRouterHandle::new(
            console_router,
        )))
        .await;

    let external_plugins_service = plugin_manager
        .service_context()
        .get_service::<temps_external_plugins::ExternalPluginsService>();
    let cloud_service = plugin_manager
        .service_context()
        .get_service::<CloudService>();

    // Join the channel to the router now that both exist. Plugins connect
    // during startup, before this router could possibly be assembled (it
    // contains the external-plugin routes), so the bridge is installed into
    // a shared slot the channel reads per call rather than passed at
    // connect time.
    if let Some(service) = external_plugins_service.clone() {
        match plugin_manager
            .service_context()
            .get_service::<temps_auth::UserService>()
        {
            Some(user_service) => {
                let bridge = Arc::new(temps_external_plugins::host_api::RouterHostApi::new(
                    plugin_api_router,
                    db.clone(),
                    cookie_crypto.clone(),
                    user_service,
                ));
                service.set_host_api(bridge).await;
                // Same key the bridge verifies with, so a token this proxy
                // mints is one the channel will accept.
                service.set_actor_crypto(cookie_crypto.clone()).await;
                info!("Plugin platform API bridge installed");
            }
            None => {
                // Say so rather than leaving the slot empty and letting every
                // plugin API call fail with a generic error later.
                tracing::warn!(
                    "UserService is not registered, so plugins cannot call the platform \
                     API over the channel; plugin API calls will be refused"
                );
            }
        }
    }

    let shutdown_signal = {
        let svc = external_plugins_service.clone();
        let cloud = cloud_service.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Console API received shutdown signal, stopping background services...");
            join_cloud_enrollment_bootstrap(enrollment_bootstrap).await;
            if let Some(service) = cloud {
                service.shutdown().await;
                info!("Managed telemetry mirror shut down");
            }
            if let Some(service) = svc {
                service.shutdown_all().await;
                info!("External plugins shut down");
            }
        }
        .shared()
    };

    match config.console_admin_address.as_deref() {
        Some(admin_addr) if !admin_addr.is_empty() => {
            // Two-listener mode: public + admin on separate addresses.
            let public_listener = TcpListener::bind(&config.console_address).await?;
            info!(
                "Console PUBLIC API server listening on {}",
                config.console_address
            );
            let admin_listener = TcpListener::bind(admin_addr).await?;
            info!("Console ADMIN API server listening on {}", admin_addr);

            // Routers, middleware, and both listeners are ready; flip
            // `/readyz` to 200. `ready_signal` already fired earlier, right
            // after plugin init -- see that call site for why the two are
            // deliberately decoupled.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);

            let public_fut = axum::serve(
                public_listener,
                public_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal.clone());

            let admin_fut = axum::serve(
                admin_listener,
                admin_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal);

            tokio::try_join!(public_fut, admin_fut)?;
        }
        _ => {
            // Single-listener mode (backwards compatible): merge public + admin
            // and serve from `console_address`. Admin gate still applies if
            // configured, but it now gates the merged surface — operators who
            // want network-layer isolation should set TEMPS_CONSOLE_ADMIN_ADDRESS.
            let merged = Router::new().merge(public_app).merge(admin_app);

            let listener = TcpListener::bind(&config.console_address).await?;
            info!("Console API server listening on {}", config.console_address);

            // Routers, middleware, and the listener are ready; flip `/readyz`
            // to 200. `ready_signal` already fired earlier, right after
            // plugin init -- see that call site for why the two are
            // deliberately decoupled.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);

            axum::serve(
                listener,
                merged.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal)
            .await?;
        }
    }

    info!("Console API server exited");
    Ok(())
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt; // for `oneshot`

    /// Send a single GET to the health router and return the status code.
    async fn get_status(ready: Arc<AtomicBool>, path: &str) -> StatusCode {
        let app = health_router(ready);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        resp.status()
    }

    #[tokio::test]
    async fn healthz_is_always_ok_regardless_of_readiness() {
        // Liveness must not depend on warmup state: a process that is serving
        // HTTP is alive even while plugins initialize.
        let not_ready = Arc::new(AtomicBool::new(false));
        assert_eq!(
            get_status(not_ready.clone(), "/healthz").await,
            StatusCode::OK
        );

        let ready = Arc::new(AtomicBool::new(true));
        assert_eq!(get_status(ready, "/healthz").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_is_503_until_ready_then_200() {
        // The upgrade health-gate depends on this: binding the port is not
        // enough — `/readyz` must stay 503 until plugin init flips the flag.
        let flag = Arc::new(AtomicBool::new(false));
        assert_eq!(
            get_status(flag.clone(), "/readyz").await,
            StatusCode::SERVICE_UNAVAILABLE,
            "readyz must report 503 while the console is warming up"
        );

        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            get_status(flag, "/readyz").await,
            StatusCode::OK,
            "readyz must report 200 once plugins are initialized"
        );
    }

    #[tokio::test]
    async fn readyz_reflects_live_flag_flips() {
        // The probe reads the shared flag on every request (it does not latch),
        // so a flag that flips back to false (e.g. a future drain signal) is
        // observed immediately.
        let flag = Arc::new(AtomicBool::new(true));
        assert_eq!(get_status(flag.clone(), "/readyz").await, StatusCode::OK);
        flag.store(false, Ordering::Relaxed);
        assert_eq!(
            get_status(flag, "/readyz").await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}

#[cfg(test)]
mod initial_admin_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn configured_initial_admin_is_optional_for_interactive_starts() {
        assert_eq!(configured_initial_admin(None, None).unwrap(), None);
    }

    #[test]
    fn optional_environment_variable_distinguishes_absent_and_non_unicode_values() {
        assert_eq!(
            optional_environment_variable_result(
                "TEMPS_ADMIN_EMAIL",
                Err(std::env::VarError::NotPresent),
            )
            .unwrap(),
            None
        );

        let result = optional_environment_variable_result(
            "TEMPS_ADMIN_EMAIL",
            Err(std::env::VarError::NotUnicode(std::ffi::OsString::from(
                "invalid-value",
            ))),
        );
        assert!(matches!(
            result,
            Err(InitialAdminConfigError::InvalidEnvironment {
                name: "TEMPS_ADMIN_EMAIL",
                ..
            })
        ));
    }

    #[test]
    fn configured_admin_email_is_trimmed_and_normalized() {
        assert_eq!(
            normalize_configured_admin_email("  Admin@Example.COM ").unwrap(),
            "admin@example.com"
        );
    }

    #[test]
    fn configured_admin_email_rejects_invalid_values() {
        let overlong_local = format!("{}@example.com", "a".repeat(65));
        let overlong_domain_label = format!("admin@{}.com", "a".repeat(64));
        for value in [
            "",
            "admin",
            "admin@example",
            "example.com",
            "a@@example.com",
            "user name@example.com",
            "admin@\n.example.com",
            ".admin@example.com",
            "admin..user@example.com",
            "admin@-example.com",
            "admin@example-.com",
            &overlong_local,
            &overlong_domain_label,
        ] {
            assert!(
                matches!(
                    normalize_configured_admin_email(value),
                    Err(InitialAdminConfigError::InvalidEmail)
                ),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn configured_initial_admin_reads_and_validates_password_secret() {
        let secret = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(secret.path(), "tT3!0123456789abcdef\n").unwrap();

        let configured =
            configured_initial_admin(Some("Admin@Example.COM"), secret.path().to_str())
                .unwrap()
                .unwrap();

        assert_eq!(configured.0, "admin@example.com");
        assert_eq!(configured.1, "tT3!0123456789abcdef");
    }

    #[test]
    fn configured_initial_admin_requires_both_values() {
        assert!(matches!(
            configured_initial_admin(Some("admin@example.com"), None),
            Err(InitialAdminConfigError::IncompleteCredentials)
        ));
        assert!(matches!(
            configured_initial_admin(None, Some("/run/secrets/admin")),
            Err(InitialAdminConfigError::IncompleteCredentials)
        ));
    }

    #[test]
    fn deleted_initial_admin_fails_closed() {
        assert!(matches!(
            ensure_existing_initial_admin_is_active(true, "admin@example.com"),
            Err(InitialAdminConfigError::DeletedUser { .. })
        ));
        assert!(ensure_existing_initial_admin_is_active(false, "admin@example.com").is_ok());
    }

    #[tokio::test]
    async fn missing_admin_role_returns_contextual_bootstrap_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<users::Model>::new()])
            .append_query_results([Vec::<temps_entities::roles::Model>::new()])
            .into_connection();

        let result =
            create_initial_admin_user(&db, "admin@example.com", Some("tT3!0123456789abcdef")).await;

        assert!(matches!(
            result,
            Err(InitialAdminBootstrapError::AdminRoleNotFound { email })
                if email == "admin@example.com"
        ));
    }

    #[tokio::test]
    async fn initial_admin_lookup_preserves_database_error_context() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("user lookup failed".to_string())])
            .into_connection();

        let result =
            create_initial_admin_user(&db, "admin@example.com", Some("tT3!0123456789abcdef")).await;

        assert!(matches!(
            result,
            Err(InitialAdminBootstrapError::LookupUser {
                email,
                source: DbErr::Custom(message),
            }) if email == "admin@example.com" && message == "user lookup failed"
        ));
    }

    #[tokio::test]
    async fn role_assignment_failure_rolls_back_initial_user_transaction() {
        let now = chrono::Utc::now();
        let role = temps_entities::roles::Model {
            id: 1,
            name: "admin".to_string(),
            created_at: now,
            updated_at: now,
        };
        let user = users::Model {
            id: 1,
            name: "Admin".to_string(),
            email: "admin@example.com".to_string(),
            password_hash: Some("unused-by-mock".to_string()),
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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<users::Model>::new()])
            .append_query_results([vec![role]])
            .append_query_results([vec![user]])
            .append_query_errors([DbErr::Custom("role assignment failed".to_string())])
            .into_connection();

        let result =
            create_initial_admin_user(&db, "admin@example.com", Some("tT3!0123456789abcdef")).await;
        assert!(matches!(
            result,
            Err(InitialAdminBootstrapError::AssignAdminRole {
                email,
                user_id: 1,
                role_id: 1,
                source: DbErr::Custom(message),
            }) if email == "admin@example.com" && message == "role assignment failed"
        ));

        let log = db.into_transaction_log();
        assert_eq!(log.len(), 3, "lookups plus one rolled-back transaction");
        let bootstrap = log[2].statements();
        assert_eq!(bootstrap.len(), 4);
        assert_eq!(bootstrap[0].sql, "BEGIN");
        assert!(bootstrap[1].sql.starts_with("INSERT INTO \"users\""));
        assert!(bootstrap[2].sql.starts_with("INSERT INTO \"user_roles\""));
        assert_eq!(
            bootstrap[3].sql, "ROLLBACK",
            "a failed role assignment must roll back the initial user insert"
        );
    }

    #[test]
    fn cloud_enrollment_code_env_is_absent_when_the_variable_is_unset() {
        assert_eq!(
            parse_cloud_enrollment_code_env(Err(std::env::VarError::NotPresent)),
            None
        );
    }

    #[test]
    fn cloud_enrollment_code_env_is_absent_when_blank() {
        assert_eq!(parse_cloud_enrollment_code_env(Ok("".to_string())), None);
        assert_eq!(parse_cloud_enrollment_code_env(Ok("   ".to_string())), None);
    }

    #[test]
    fn cloud_enrollment_code_env_degrades_to_absent_on_non_unicode_value() {
        // Must not panic and must not surface as "present" -- the caller
        // treats this exactly like the variable being unset, after logging a
        // warning explaining why.
        assert_eq!(
            parse_cloud_enrollment_code_env(Err(std::env::VarError::NotUnicode(
                std::ffi::OsString::from("invalid-value")
            ))),
            None
        );
    }

    #[test]
    fn cloud_enrollment_code_env_is_present_when_set() {
        assert_eq!(
            parse_cloud_enrollment_code_env(Ok("ABCD-EFGH".to_string())),
            Some("ABCD-EFGH".to_string())
        );
    }

    /// What one bootstrap run did, step by step, as seen by the injected
    /// hooks: which network steps were attempted and which audit rows would
    /// have been written.
    #[derive(Default)]
    struct BootstrapProbe {
        enroll_called: AtomicBool,
        link_audited: AtomicBool,
        provision_called: AtomicBool,
        backup_audited: std::sync::Mutex<Option<ManagedBackupOutcome>>,
    }

    impl BootstrapProbe {
        fn backup_audited(&self) -> Option<ManagedBackupOutcome> {
            self.backup_audited
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    type EnrollResult = Result<FirstLinkEnrollment, CloudServiceError>;

    fn established() -> EnrollResult {
        Ok(FirstLinkEnrollment::Established(
            temps_cloud_client::EnrollmentKind::First,
        ))
    }

    /// Drive `run_cloud_enrollment_bootstrap` with every hook wired to
    /// `probe`, using the given enroll and provision futures.
    async fn drive_bootstrap<EnrollFut, ProvisionFut>(
        probe: &Arc<BootstrapProbe>,
        enroll: impl FnOnce() -> EnrollFut,
        provision: impl FnOnce() -> ProvisionFut,
    ) where
        EnrollFut: std::future::Future<Output = EnrollResult>,
        ProvisionFut: std::future::Future<Output = ManagedBackupOutcome>,
    {
        let enroll_probe = probe.clone();
        let link_probe = probe.clone();
        let provision_probe = probe.clone();
        let backup_probe = probe.clone();
        let enroll_fut = enroll();
        let provision_fut = provision();
        run_cloud_enrollment_bootstrap(
            "the-code",
            move |code| async move {
                assert_eq!(code, "the-code");
                enroll_probe.enroll_called.store(true, Ordering::SeqCst);
                enroll_fut.await
            },
            move || async move {
                link_probe.link_audited.store(true, Ordering::SeqCst);
            },
            move || async move {
                provision_probe
                    .provision_called
                    .store(true, Ordering::SeqCst);
                provision_fut.await
            },
            move |outcome| async move {
                *backup_probe
                    .backup_audited
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn unattended_cloud_enrollment_succeeds_with_a_valid_code() {
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            || async { established() },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(probe.enroll_called.load(Ordering::SeqCst));
        assert!(
            probe.link_audited.load(Ordering::SeqCst),
            "a persisted link must be audited"
        );
        assert!(probe.provision_called.load(Ordering::SeqCst));
        assert!(
            matches!(
                probe.backup_audited(),
                Some(ManagedBackupOutcome::Provisioned)
            ),
            "the provisioned backup credential must be audited with its outcome"
        );
    }

    #[tokio::test]
    async fn unattended_cloud_enrollment_degrades_to_a_warning_on_an_invalid_code() {
        let probe = Arc::new(BootstrapProbe::default());
        // The important assertion is what does NOT happen: no panic, no
        // propagated error -- server startup must continue exactly as if the
        // variable had never been set -- no backup provisioning against a
        // link that does not exist, and no audit row claiming one.
        drive_bootstrap(
            &probe,
            || async {
                Err(CloudServiceError::InvalidBackend {
                    reason: "enrollment code expired".to_string(),
                })
            },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(
            probe.enroll_called.load(Ordering::SeqCst),
            "enrollment must still be attempted before failing"
        );
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
        assert!(probe.backup_audited().is_none());
    }

    #[tokio::test]
    async fn unattended_cloud_enrollment_is_a_silent_no_op_when_already_linked() {
        // The linked decision is the service's, made atomically with the
        // enrollment; what the bootstrap owes is to persist and audit
        // nothing when told the instance was already linked.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            || async { Ok(FirstLinkEnrollment::AlreadyLinked) },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
        assert!(probe.backup_audited().is_none());
    }

    #[tokio::test]
    async fn unattended_cloud_enrollment_that_lost_the_race_persists_and_audits_nothing() {
        // An operator enrolled while the environment code was in flight and
        // that link stands. This task wrote no credential, so it must not
        // write a CLOUD_LINK_CONNECTED row of its own (the operator's request
        // recorded theirs) and must not provision backups for a link it does
        // not own.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            || async { Ok(FirstLinkEnrollment::LostRaceToConcurrentEnrollment) },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(probe.enroll_called.load(Ordering::SeqCst));
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
        assert!(probe.backup_audited().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn unattended_cloud_enrollment_gives_up_on_a_backend_that_never_answers() {
        // A Cloud backend that accepts the connection and then never replies
        // to the code redemption. The bootstrap must not hang the task
        // forever on it: after CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT it logs and
        // returns, having persisted nothing and therefore audited nothing.
        // Paused time makes the wait instantaneous in the test.
        let probe = Arc::new(BootstrapProbe::default());
        let bootstrap = drive_bootstrap(&probe, std::future::pending::<EnrollResult>, || async {
            ManagedBackupOutcome::Provisioned
        });

        tokio::time::timeout(
            CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT + std::time::Duration::from_secs(1),
            bootstrap,
        )
        .await
        .expect("the bootstrap must return once its own timeout elapses");

        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn a_persisted_link_is_audited_even_if_backup_provisioning_never_answers() {
        // The case a security audit of this bootstrap called out: the code
        // is redeemed and the credential persisted, then the *second*
        // network round-trip (managed backup credential) hangs. The link's
        // audit row must already be written before that step began, and the
        // timeout on that step must not undo it.
        let probe = Arc::new(BootstrapProbe::default());
        let bootstrap = drive_bootstrap(
            &probe,
            || async { established() },
            std::future::pending::<ManagedBackupOutcome>,
        );

        tokio::time::timeout(
            CLOUD_ENROLLMENT_BOOTSTRAP_TIMEOUT + std::time::Duration::from_secs(1),
            bootstrap,
        )
        .await
        .expect("the bootstrap must return once the backup step's timeout elapses");

        assert!(
            probe.link_audited.load(Ordering::SeqCst),
            "CLOUD_LINK_CONNECTED must be recorded before the backup step runs"
        );
        assert!(probe.provision_called.load(Ordering::SeqCst));
        assert!(
            probe.backup_audited().is_none(),
            "a backup step that never completed persisted nothing to audit"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_waits_for_an_in_flight_enrollment_but_only_so_long() {
        // A task that finishes within the grace period is joined...
        let quick = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });
        tokio::time::timeout(
            CLOUD_ENROLLMENT_SHUTDOWN_GRACE,
            join_cloud_enrollment_bootstrap(Some(quick)),
        )
        .await
        .expect("a quick task is joined within the grace period");

        // ...and one wedged on the network does not hold shutdown hostage.
        let wedged = tokio::spawn(std::future::pending::<()>());
        tokio::time::timeout(
            CLOUD_ENROLLMENT_SHUTDOWN_GRACE + std::time::Duration::from_secs(1),
            join_cloud_enrollment_bootstrap(Some(wedged)),
        )
        .await
        .expect("shutdown must give up on a wedged enrollment after the grace period");

        // No task at all is a no-op.
        join_cloud_enrollment_bootstrap(None).await;
    }
}

#[cfg(test)]
mod ai_tool_allowlist_tests {
    use super::*;
    use temps_ai_api_tools::ReadOnlyApiIndex;
    use temps_providers::handlers::metrics_handlers::MetricsApiDoc;
    use temps_providers::handlers::ExternalServiceApiDoc;
    use utoipa::OpenApi;

    /// A throwaway admin auth context for the prepare-path tests (no request is
    /// executed, so it only has to satisfy the advisory permission filter).
    fn admin_auth() -> temps_auth::AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 1,
            name: "tester".to_string(),
            email: "tester@internal".to_string(),
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
        temps_auth::AuthContext::new_session(user, temps_auth::permissions::Role::Admin)
    }

    /// End-to-end against the REAL `OtelApiDoc`: a `create_alert` proposal with
    /// a malformed `detection_config` must be rejected while the model can
    /// still fix it, not after a human has approved it.
    ///
    /// This is the exact payload a live model produced three runs running
    /// (`{"type": "threshold"}` instead of `{"kind": "static", …}`). Before the
    /// shape check it validated cleanly, was staged as a pending action, and
    /// failed with a 422 only once the user clicked Confirm — the one person in
    /// the loop who had no way to know it was wrong.
    #[tokio::test]
    async fn malformed_detection_config_is_rejected_before_a_human_sees_it() {
        use temps_ai_api_tools::{ApiCallScope, InternalApiCaller, WritePrepareOutcome};
        use utoipa::OpenApi;

        let openapi = temps_otel::plugin::OtelApiDoc::openapi();
        // No request is executed on the prepare path, so an empty router is fine.
        let caller = InternalApiCaller::new_write_allowlisted(
            axum::Router::new(),
            &openapi,
            vec!["create_alert".to_string()],
        );
        let scope = ApiCallScope {
            auth: admin_auth(),
            project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        };

        let base = "alerts create_alert --name p95 --metric_name http.server.duration \
                    --aggregation avg --window_secs 300 --for_duration_secs 300 \
                    --severity critical --enabled true";

        // 1. The model's actual mistake: no `kind` discriminator.
        let outcome = caller.prepare_write_cli(
            &format!("{base} --detection_config '{{\"type\": \"threshold\", \"threshold\": 500}}'"),
            &scope,
        );
        let WritePrepareOutcome::Invalid(msg) = outcome else {
            panic!("a payload the API rejects with 422 must not be staged for approval");
        };
        assert!(msg.contains("kind"), "must name the discriminator: {msg}");
        assert!(
            msg.contains("static"),
            "must show the accepted variants — the model does not run --help: {msg}"
        );

        // 2. A quoted number and a natural-but-wrong comparator: both present,
        //    both fatal, both used to reach the human before failing.
        let outcome = caller.prepare_write_cli(
            &format!(
                "{base} --detection_config \
                 '{{\"kind\": \"static\", \"comparator\": \">\", \"threshold\": \"0.5\"}}'"
            ),
            &scope,
        );
        let WritePrepareOutcome::Invalid(msg) = outcome else {
            panic!("a payload the API rejects with 422 must not be staged for approval");
        };
        assert!(
            msg.contains("gt") && msg.contains("lte"),
            "the real Comparator enum must be listed: {msg}"
        );

        // 3. The correct payload still validates and is staged.
        let outcome = caller.prepare_write_cli(
            &format!(
                "{base} --detection_config \
                 '{{\"kind\": \"static\", \"comparator\": \"gt\", \"threshold\": 500}}'"
            ),
            &scope,
        );
        match outcome {
            WritePrepareOutcome::Prepared(_) => {}
            WritePrepareOutcome::Invalid(message) => {
                panic!("a well-formed static detector must still be proposable: {message}")
            }
            WritePrepareOutcome::Help(message) => {
                panic!("a well-formed static detector unexpectedly returned help: {message}")
            }
        }
    }

    /// `preview_alert` must be reachable from the read CLI, and its help must
    /// describe a backtest the model can actually run.
    ///
    /// Allowlisted is not the same as usable. It was discoverable all along —
    /// the reason it was never called is that it rejected every detector kind
    /// except `anomaly`, while every rule the model proposes is a static
    /// threshold. It also advertised its optional RFC 3339 timestamps as
    /// `<array>`, because a nullable type union was being read as an array.
    #[tokio::test]
    async fn preview_alert_is_usable_from_the_read_cli() {
        use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};
        use utoipa::OpenApi;

        let openapi = temps_otel::plugin::OtelApiDoc::openapi();
        let caller = InternalApiCaller::new_allowlisted_with_safe_posts(
            axum::Router::new(),
            &openapi,
            ai_read_allowlist(),
            ai_read_safe_posts(),
        );
        let auth = admin_auth();
        let scope = ApiCallScope {
            auth: auth.clone(),
            project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        };

        // Discoverable by browsing, which is how the model finds anything.
        let section = caller.run_cli("alerts --help", &scope).await;
        assert!(
            section.contains("preview_alert"),
            "must be listed in its section: {section}"
        );

        let help = caller.run_cli("alerts preview_alert --help", &scope).await;

        // The static variant has to be offered, or the backtest is impossible
        // for the rules this flow actually proposes.
        assert!(
            help.contains("\"kind\": \"static\""),
            "static detectors must be backtestable: {help}"
        );
        assert!(
            help.contains("comparator") && help.contains("gt|gte|lt|lte"),
            "the static variant's fields must be spelled out: {help}"
        );

        // Optional timestamps are strings, not lists.
        assert!(
            help.contains("--start_time <string>"),
            "an Option<String> must not advertise itself as an array: {help}"
        );
        assert!(
            !help.contains("--end_time <array>"),
            "nullable != array: {help}"
        );
    }

    /// The read-only-POST list and the write allowlist must stay disjoint.
    ///
    /// This is the one rule holding up the safe-POST mechanism, and until now it
    /// existed only as prose in a doc comment. `create_alert` and
    /// `preview_alert` are neighbours in the same handler module under the same
    /// OpenAPI tag, so a future `preview_and_save_alert` added by pattern-match
    /// would execute unconfirmed writes with the chat user's auth and no confirm
    /// card. Turn the rule into a build failure.
    #[test]
    fn safe_post_and_write_allowlists_are_disjoint() {
        let safe: std::collections::HashSet<String> = ai_read_safe_posts().into_iter().collect();
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();

        let both: Vec<&String> = safe.intersection(&writes).collect();
        assert!(
            both.is_empty(),
            "an operation cannot be both a side-effect-free read and a vetted write: {both:?}"
        );
    }

    #[test]
    fn automatic_deployment_repairs_are_proposable() {
        let writes: std::collections::HashSet<String> = ai_write_allowlist().into_iter().collect();

        for operation in [
            "update_automatic_deploy",
            "update_environment_settings",
            "update_git_settings",
            "reinstall_gitlab_webhook",
        ] {
            assert!(
                writes.contains(operation),
                "automatic-deployment diagnosis cannot stage `{operation}`"
            );
        }
    }

    #[test]
    fn application_workspace_drop_is_proposable() {
        let openapi = temps_ai_chat::handlers::AiChatApiDoc::openapi();
        let caller = temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
            axum::Router::new(),
            &openapi,
            ai_write_allowlist(),
        );

        assert!(caller
            .indexed_operation_ids()
            .contains(&"deploy_application_workspace_project".to_string()));
    }

    #[test]
    fn global_notification_operations_are_available_to_workspace_chats() {
        use utoipa::OpenApi;

        let openapi = temps_notifications::NotificationProvidersApiDoc::openapi();
        let writes = temps_ai_api_tools::InternalApiCaller::new_write_allowlisted(
            axum::Router::new(),
            &openapi,
            ai_write_allowlist(),
        )
        .indexed_operation_ids();
        for operation in [
            "create_notification_provider",
            "update_notification_provider",
            "delete_notification_provider",
            "create_notification_route",
            "update_notification_route",
            "delete_notification_route",
        ] {
            assert!(
                writes.contains(&operation.to_string()),
                "missing {operation}"
            );
        }

        let reads = temps_ai_api_tools::InternalApiCaller::new_allowlisted(
            axum::Router::new(),
            &openapi,
            ai_read_allowlist(),
        )
        .indexed_operation_ids();
        assert!(reads.contains(&"list_notification_routes".to_string()));
        assert!(reads.contains(&"get_notification_route".to_string()));
        assert!(reads.contains(&"list_notification_providers".to_string()));
        assert!(reads.contains(&"get_notification_provider".to_string()));
    }

    /// Every safe-POST entry must resolve to a real operation, or it silently
    /// does nothing — the same failure mode the read-allowlist test guards.
    #[test]
    fn safe_posts_resolve_against_the_real_openapi() {
        use temps_ai_api_tools::ReadOnlyApiIndex;
        use utoipa::OpenApi;

        let openapi = temps_otel::plugin::OtelApiDoc::openapi();
        let safe = ai_read_safe_posts();
        let safe_refs: Vec<&str> = safe.iter().map(String::as_str).collect();
        let index =
            ReadOnlyApiIndex::from_openapi_allowlist_with_safe_posts(&openapi, &[], &safe_refs);

        for op in &safe {
            assert!(
                index.get(op).is_some(),
                "`{op}` is allowlisted as a read-only POST but does not resolve — \
                 check for a typo or a renamed handler"
            );
        }
    }

    /// The AI read allowlist must never contain duplicate entries — a repeat
    /// is dead weight in the model's tool catalogue and a signal something
    /// was pasted twice while merging.
    #[test]
    fn ai_read_allowlist_has_no_duplicate_entries() {
        let allowlist = ai_read_allowlist();
        let mut seen = std::collections::HashSet::new();
        for entry in &allowlist {
            assert!(
                seen.insert(entry.as_str()),
                "duplicate allowlist entry: {entry}"
            );
        }
    }

    #[tokio::test]
    async fn get_projects_resolves_and_is_discoverable_in_the_read_cli() {
        use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

        let openapi = temps_projects::handlers::ApiDoc::openapi();
        let caller =
            InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
        assert!(caller
            .indexed_operation_ids()
            .contains(&"get_projects".to_string()));

        let scope = ApiCallScope {
            auth: admin_auth(),
            project_scope: temps_ai_api_tools::ProjectSelectorScope::Unrestricted,
        };
        let catalog = caller.run_cli("projects --help", &scope).await;
        assert!(catalog.contains("get_projects"), "catalog: {catalog}");
    }

    /// Reproduces the page-country chat failure against the real analytics
    /// OpenAPI document and the production AI read allowlist. This catches a
    /// renamed/removed handler, missing allowlist entry, or parameter drift in
    /// addition to the virtual CLI's unknown-operation recovery behavior.
    #[tokio::test]
    async fn page_country_analytics_recovers_through_real_api_contract() {
        use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

        let openapi = temps_analytics::handler::AnalyticsApiDoc::openapi();
        let caller =
            InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
        let scope = ApiCallScope {
            auth: admin_auth(),
            project_scope: temps_ai_api_tools::ProjectSelectorScope::Allowed(vec![1]),
        };

        let recovery = caller
            .run_cli("analytics get_analytics --path /managed", &scope)
            .await;
        assert!(
            recovery.contains("get_page_path_detail"),
            "page-level aggregate missing from recovery help: {recovery}"
        );

        let help = caller
            .run_cli("analytics get_page_path_detail --help", &scope)
            .await;
        for flag in ["--page_path", "--start_date", "--end_date"] {
            assert!(
                help.contains(flag),
                "real page-detail contract is missing `{flag}`: {help}"
            );
        }
    }

    #[test]
    fn test_ai_read_allowlist_api_traffic_exposes_only_privacy_safe_operations() {
        let openapi = temps_analytics::handler::AnalyticsApiDoc::openapi();
        let allowlist = ai_read_allowlist();
        let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
        let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

        assert!(
            index.get("get_api_timeseries").is_some(),
            "AI chat must discover the privacy-safe API traffic time series"
        );

        for operation in ["get_api_summary", "get_api_routes", "get_api_callers"] {
            assert!(
                !allowlist.iter().any(|entry| entry == operation),
                "paid or sensitive API traffic operation must not enter the AI read allowlist: `{operation}`"
            );
            assert!(
                index.get(operation).is_none(),
                "paid or sensitive API traffic operation must remain invisible to AI discovery: `{operation}`"
            );
        }
    }

    #[tokio::test]
    async fn list_services_resolves_and_is_discoverable_in_the_read_cli() {
        use temps_ai_api_tools::{ApiCallScope, InternalApiCaller};

        let openapi = ExternalServiceApiDoc::openapi();
        let caller =
            InternalApiCaller::new_allowlisted(axum::Router::new(), &openapi, ai_read_allowlist());
        assert!(caller
            .indexed_operation_ids()
            .contains(&"list_services".to_string()));

        let scope = ApiCallScope {
            auth: admin_auth(),
            project_scope: temps_ai_api_tools::ProjectSelectorScope::Unrestricted,
        };
        let catalog = caller.run_cli("external-services --help", &scope).await;
        assert!(catalog.contains("list_services"), "catalog: {catalog}");
    }

    /// PR #265 added `DeploymentMetricsGetRange`/`DeploymentMetricsGetLatest`/
    /// `NodeMetricsGetRange` to the allowlist as bare strings — nothing
    /// type-checks them against the real `operation_id`s declared via
    /// `#[utoipa::path]` in `metrics_handlers.rs`. This proves each one
    /// actually resolves through the same
    /// `ReadOnlyApiIndex::from_openapi_allowlist` the production
    /// `InternalApiCaller` uses, so a future typo or renamed handler fails a
    /// test instead of silently vanishing from the AI's tool catalogue — the
    /// exact failure mode PR #265 itself was fixing (see the allowlist doc
    /// comment on `ai_read_allowlist`).
    #[test]
    fn deployment_and_node_metrics_tools_resolve_against_real_openapi() {
        let openapi = MetricsApiDoc::openapi();
        let allowlist = ai_read_allowlist();
        let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
        let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

        for tool in [
            "DeploymentMetricsGetRange",
            "DeploymentMetricsGetLatest",
            "NodeMetricsGetRange",
            "NodeMetricsGetLatest",
        ] {
            assert!(
                index.get(tool).is_some(),
                "AI read allowlist claims to expose `{tool}` but it does not resolve \
                 against the real MetricsApiDoc operation_id — check for a typo or a \
                 renamed handler"
            );
        }
    }

    /// The companion write operation `DeploymentMetricsToggle` (a PATCH) must
    /// never become callable through the read-only index, even though it is
    /// declared in the same `MetricsApiDoc` — `from_openapi_allowlist` only
    /// ever indexes `GET` operations, so this is a structural guarantee, not
    /// just an allowlist-membership check. Matches the PR #265 description's
    /// own stated intent: "DeploymentMetricsToggle ... is intentionally left
    /// out, consistent with the existing pattern of excluding all write
    /// operations from the read allowlist."
    #[test]
    fn deployment_metrics_toggle_write_op_is_never_read_callable() {
        let openapi = MetricsApiDoc::openapi();
        let allowlist = ai_read_allowlist();
        let allowlist_refs: Vec<&str> = allowlist.iter().map(String::as_str).collect();
        let index = ReadOnlyApiIndex::from_openapi_allowlist(&openapi, &allowlist_refs);

        assert!(
            index.get("DeploymentMetricsToggle").is_none(),
            "DeploymentMetricsToggle is a write (PATCH) operation and must never be \
             resolvable via the read-only AI tool allowlist"
        );
    }

    /// `describe_api` only ever surfaces an operation's `summary`/`description`
    /// to the model — never response-body field docs — so a span's
    /// `duration_ms` vs. unlabeled `attributes` units can only be explained via
    /// the operation description itself. This proves the unit-guidance text
    /// added to the trace/GenAI-trace handler doc comments actually survives
    /// into the real compiled `OtelApiDoc` and would reach the model through
    /// `describe_api`, rather than just existing as a comment nobody reads.
    #[test]
    fn trace_tool_descriptions_warn_about_unlabeled_attribute_units() {
        let openapi = temps_otel::plugin::OtelApiDoc::openapi();
        let index = ReadOnlyApiIndex::from_openapi(&openapi, &[]);

        for operation_id in [
            "get_trace",
            "query_traces",
            "query_genai_traces",
            "get_genai_trace",
        ] {
            let op = index
                .get(operation_id)
                .unwrap_or_else(|| panic!("{operation_id} missing from OtelApiDoc"));
            let description = op.description.as_deref().unwrap_or_default();
            assert!(
                description.contains("duration_ms") && description.contains("milliseconds"),
                "{operation_id}'s OpenAPI description must warn the model that only \
                 `duration_ms` is guaranteed to be milliseconds and other numeric \
                 fields carry unlabeled/different units — got: {description:?}"
            );
        }
    }
}

#[cfg(test)]
mod error_telemetry_tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use temps_core::error_metrics::{self, CATEGORY_HTTP_5XX};
    use tower::ServiceExt;

    async fn failing_handler() -> axum::response::Response {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "it broke, with details that must never reach telemetry",
        )
            .into_response()
    }

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn get_request(uri: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("valid test request")
    }

    /// The middleware must count 5xx responses under the route TEMPLATE (the
    /// compile-time string from our route table), never the concrete request
    /// path, and must not count non-5xx responses at all. Route names are
    /// unique to this test so parallel tests can't interfere via the global
    /// counter store.
    #[tokio::test]
    async fn track_server_errors_counts_5xx_by_route_template_only() {
        let app = Router::new()
            .route("/error-telemetry-test/{id}", get(failing_handler))
            .route("/error-telemetry-test-ok", get(ok_handler))
            .layer(axum::middleware::from_fn(track_server_errors));

        let response = app
            .clone()
            .oneshot(get_request("/error-telemetry-test/12345"))
            .await
            .expect("request succeeds");
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );

        let response = app
            .oneshot(get_request("/error-telemetry-test-ok"))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let counters = error_metrics::global();
        assert_eq!(
            counters.count_for(CATEGORY_HTTP_5XX, "GET /error-telemetry-test/{id} 500"),
            1,
            "5xx must be recorded under the route template"
        );
        assert_eq!(
            counters.count_for(CATEGORY_HTTP_5XX, "GET /error-telemetry-test/12345 500"),
            0,
            "the concrete request path must never be recorded"
        );
        assert_eq!(
            counters.count_for(CATEGORY_HTTP_5XX, "GET /error-telemetry-test-ok 200"),
            0,
            "non-5xx responses must not be recorded"
        );
    }

    /// Requests that don't match any route (SPA fallback and friends) are
    /// recorded under the fixed `unmatched` label — visible, but without
    /// capturing the raw path.
    #[tokio::test]
    async fn track_server_errors_uses_unmatched_label_for_fallback() {
        async fn failing_fallback() -> axum::response::Response {
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
        }

        let app = Router::new()
            .fallback(failing_fallback)
            .layer(axum::middleware::from_fn(track_server_errors));

        let before = error_metrics::global().count_for(CATEGORY_HTTP_5XX, "GET unmatched 500");
        let response = app
            .oneshot(get_request("/error-telemetry-secret-user-path"))
            .await
            .expect("request succeeds");
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );

        let counters = error_metrics::global();
        assert_eq!(
            counters.count_for(CATEGORY_HTTP_5XX, "GET unmatched 500"),
            before + 1
        );
        assert_eq!(
            counters.count_for(
                CATEGORY_HTTP_5XX,
                "GET /error-telemetry-secret-user-path 500"
            ),
            0,
            "unmatched raw paths must never be recorded"
        );
    }

    /// The error_summary event must carry only counts and identifier keys —
    /// with per-category totals, a capped top list, and overflow present only
    /// when keys were actually dropped.
    #[test]
    fn build_error_summary_event_shape() {
        use temps_core::error_metrics::{ErrorCount, ErrorSummary};

        let summary = ErrorSummary {
            total: 7,
            overflow: 0,
            category_totals: vec![("http_5xx", 2), ("log_error", 5)],
            top: vec![
                ErrorCount {
                    category: "log_error",
                    key: "temps_backup::service".to_string(),
                    count: 5,
                },
                ErrorCount {
                    category: "http_5xx",
                    key: "GET /api/projects/{id} 500".to_string(),
                    count: 2,
                },
            ],
        };

        let event = build_error_summary_event(&summary);
        assert_eq!(event.event_type, "error_summary");
        assert_eq!(event.properties["total"], serde_json::json!(7));
        assert_eq!(event.properties["log_error_total"], serde_json::json!(5));
        assert_eq!(event.properties["http_5xx_total"], serde_json::json!(2));
        assert!(
            !event.properties.contains_key("overflow"),
            "overflow must be omitted when zero"
        );

        let top = event.properties["top"].as_array().expect("top is an array");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0]["key"], serde_json::json!("temps_backup::service"));
        assert_eq!(top[0]["count"], serde_json::json!(5));
    }

    #[test]
    fn build_error_summary_event_reports_overflow_when_capped() {
        use temps_core::error_metrics::ErrorSummary;

        let summary = ErrorSummary {
            total: 10,
            overflow: 3,
            category_totals: vec![("log_error", 7)],
            top: vec![],
        };

        let event = build_error_summary_event(&summary);
        assert_eq!(event.properties["overflow"], serde_json::json!(3));
    }
}

#[cfg(test)]
mod log_storage_config_tests {
    use super::*;
    use std::sync::Mutex;

    /// `std::env` is process-global; these tests mutate it, so they must not
    /// interleave with each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const S3_VARIABLES: [&str; 3] = [
        "TEMPS_LOG_S3_BUCKET",
        "TEMPS_LOG_S3_ACCESS_KEY_ID",
        "TEMPS_LOG_S3_SECRET_ACCESS_KEY",
    ];

    fn clear_log_storage_env() {
        std::env::remove_var("TEMPS_LOG_STORAGE_BACKEND");
        for variable in S3_VARIABLES {
            std::env::remove_var(variable);
        }
    }

    /// Holds the lock AND guarantees the environment is clean again.
    ///
    /// Cleaning up at the end of each test body only works while every test
    /// passes: a failed assertion unwinds straight past it and leaks
    /// `TEMPS_LOG_STORAGE_BACKEND=s3` into whichever test takes the lock next,
    /// turning one real failure into a cascade of unrelated ones. Cleanup
    /// belongs in `Drop`, which runs on the unwind path too.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            clear_log_storage_env();
            Self { _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            clear_log_storage_env();
        }
    }

    #[test]
    fn defaults_to_the_filesystem_backend() {
        let _guard = EnvGuard::acquire();

        let config = log_aggregator_storage_config(std::path::Path::new("/srv/temps"))
            .expect("the filesystem backend needs no configuration");

        match config {
            StorageConfig::Filesystem { base_path } => {
                assert_eq!(base_path, std::path::Path::new("/srv/temps/log-aggregator"));
            }
            other => panic!("expected the filesystem backend, got {other:?}"),
        }
    }

    #[test]
    fn s3_backend_missing_a_variable_is_an_error_not_a_panic() {
        let _guard = EnvGuard::acquire();
        std::env::set_var("TEMPS_LOG_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-logs");
        // TEMPS_LOG_S3_ACCESS_KEY_ID deliberately unset.

        let error = log_aggregator_storage_config(std::path::Path::new("/srv/temps"))
            .expect_err("an incomplete S3 configuration must be reported");

        let rendered = error.to_string();
        assert!(
            rendered.contains("TEMPS_LOG_S3_ACCESS_KEY_ID"),
            "{rendered}"
        );
        // The remedy has to be in the message: this is the only place an
        // operator sees it.
        assert!(rendered.contains("TEMPS_LOG_STORAGE_BACKEND"), "{rendered}");
    }

    #[test]
    fn a_blank_variable_counts_as_missing() {
        let _guard = EnvGuard::acquire();
        std::env::set_var("TEMPS_LOG_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "   ");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let error = log_aggregator_storage_config(std::path::Path::new("/srv/temps"))
            .expect_err("a whitespace-only bucket name is not a bucket name");

        assert!(error.to_string().contains("TEMPS_LOG_S3_BUCKET"));
    }

    #[test]
    fn complete_s3_configuration_is_accepted() {
        let _guard = EnvGuard::acquire();
        std::env::set_var("TEMPS_LOG_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-logs");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let config = log_aggregator_storage_config(std::path::Path::new("/srv/temps"))
            .expect("all required variables are present");

        match config {
            StorageConfig::S3 { bucket, region, .. } => {
                assert_eq!(bucket, "temps-logs");
                assert_eq!(region, "us-east-1", "region falls back to a default");
            }
            other => panic!("expected the S3 backend, got {other:?}"),
        }
    }
}
