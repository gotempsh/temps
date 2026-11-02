// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use argon2::{Argon2, PasswordHasher};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
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
use temps_ai_agents::AgentsPlugin;
use temps_ai_api_tools::allowlists::{ai_read_allowlist, ai_read_safe_posts, ai_write_allowlist};
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
    BootstrapBackendUrlOutcome, CloudEnrollmentActor, CloudPlugin, CloudService, CloudServiceError,
    ConsentTrace, ConsoleAccessConsent, ConsoleOidcBootstrapError, ConsoleOidcBootstrapOutcome,
    ManagedBackupOutcome, CONSOLE_OIDC_BOOTSTRAP_FILENAME,
};
use temps_cloud_client::FirstLinkEnrollment;
use temps_config::ServerConfig;
use temps_config_handlers::ConfigPlugin;
use temps_core::plugin::{PluginManager, TempsPlugin};
use temps_product_telemetry::TelemetryPlugin;
// `TempsPlugin` is used both directly (extra_plugins field) and through
// `dyn TempsPlugin` in `ConsoleApiParams`.
use super::static_files::StaticFilesPlugin;
use temps_core::templates::TemplateService;
use temps_core::{CookieCrypto, EncryptionService};
use temps_database::DbConnection;
use temps_deployer::plugin::DeployerPlugin;
use temps_deployer::traefik_discovery::DriftAlarmSink;
use temps_deployment_logs::LogsPlugin;
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
use temps_mcp_server::{McpHandlerState, McpServerPlugin};
use temps_monitoring::{
    AlarmService, ContainerHealthConfig, ContainerHealthMonitor, ContainerRuntimeResolver,
    DiskSpaceMonitor, MonitoringPlugin, OutageDetectionService,
};
use temps_notifications::NotificationsPlugin;
use temps_observability::ObservabilityPlugin;
use temps_otel::plugin::OtelPlugin;
use temps_project_groups::ProjectGroupsPlugin;
use temps_projects::ProjectsPlugin;
use temps_providers::ProvidersPlugin;
use temps_proxy::ProxyPlugin;
use temps_queue::QueuePlugin;
use temps_revenue::RevenuePlugin;
use temps_sandbox::plugin::SandboxPlugin;
use temps_screenshots::ScreenshotsPlugin;
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
    failover_due_nodes, notify_nodes_offline, refresh_control_plane_metrics,
    restore_recovered_replicas, HeartbeatWatch,
};
use temps_deployments::services::node_service::NodeService;
use utoipa_swagger_ui::SwaggerUi;

// Embed the dist directory at compile time
static WEBSITE: Dir = include_dir!("$CARGO_MANIFEST_DIR/dist");

/// Optional replacement UI bundle supplied by an embedding binary (EE or
/// another downstream build) via [`crate::set_embedded_ui`] before `dispatch`. When
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
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        // The first tick completes immediately; skip it so the first heartbeat
        // lands one full interval after boot (boot is already covered by
        // instance_started).
        interval.tick().await;
        loop {
            interval.tick().await;
            emit_heartbeat(reporter.as_ref(), db.as_ref()).await;
        }
    });
}

/// One heartbeat tick. Checks the *current* telemetry state first: an admin
/// can turn telemetry off (or on) at runtime from Settings > Telemetry, so the
/// loop always runs and each tick decides. When disabled nothing is built or
/// sent -- not even the count queries run. Returns whether an event was sent.
async fn emit_heartbeat(
    reporter: &dyn temps_core::telemetry::TelemetryReporter,
    db: &sea_orm::DatabaseConnection,
) -> bool {
    use temps_core::telemetry::TelemetryEventKind;

    if !reporter.is_enabled() {
        return false;
    }
    let event = build_instance_event(TelemetryEventKind::InstanceHeartbeat, db).await;
    reporter.report(event);
    tracing::debug!("emitted anonymous instance_heartbeat telemetry event");
    true
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
            // Always drain, so counts accumulated while telemetry was off are
            // discarded rather than reported after an admin turns it back on.
            let Some(summary) = temps_core::error_metrics::global().drain() else {
                continue;
            };
            if !reporter.is_enabled() {
                continue;
            }
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
pub(crate) enum InitialAdminConfigError {
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

/// `TEMPS_CLOUD_BACKEND_URL` -- a first-boot bootstrap *input*, companion to
/// [`TEMPS_CLOUD_ENROLLMENT_CODE_VAR`], not a runtime setting.
///
/// Today an enrollment code always redeems against the default
/// `cloud.backend_url` (`https://app.temps.sh`), so a Cloud-provisioned
/// instance has no way to enroll against a staging or self-hosted Temps
/// Cloud without an operator visiting Settings > Cloud first -- defeating the
/// point of unattended provisioning. This variable lets the same automated
/// flow that mints the enrollment code also point the instance at the
/// backend that minted it, one time, before enrollment runs.
///
/// Like the enrollment code, this is not configuration in the sense
/// CLAUDE.md's "no env vars for configuration" rule forbids: it is consumed
/// exactly once, and its *result* -- `cloud.backend_url`, set via
/// [`temps_cloud::CloudService::apply_bootstrap_backend_url`] -- is what gets
/// persisted and audited (`CLOUD_BACKEND_URL_BOOTSTRAPPED`), not the variable
/// itself. Nothing re-reads the environment variable after this boot.
///
/// Only takes effect together with [`TEMPS_CLOUD_ENROLLMENT_CODE_VAR`]: a
/// backend URL with no code to redeem against it, or on an instance that is
/// already linked, is logged and ignored rather than silently changing where
/// a *future* manual enrollment would point -- see
/// [`run_cloud_enrollment_bootstrap`]. An invalid URL (wrong scheme, a
/// non-loopback host over plain HTTP) degrades to a warning and skips
/// enrollment entirely, rather than falling back to the default backend:
/// enrolling this instance's code against the wrong Cloud tenant is worse
/// than not enrolling it at all.
const TEMPS_CLOUD_BACKEND_URL_VAR: &str = "TEMPS_CLOUD_BACKEND_URL";

/// Interprets a raw `std::env::var(TEMPS_CLOUD_BACKEND_URL_VAR)` result.
///
/// Mirrors [`parse_cloud_enrollment_code_env`] exactly (blank and
/// non-unicode both degrade to absent, with a warning on the latter) for the
/// same reason: a test drives every outcome through a synthetic `Result`
/// rather than mutating the real process environment.
fn parse_cloud_backend_url_env(result: Result<String, std::env::VarError>) -> Option<String> {
    match result {
        Ok(url) if !url.trim().is_empty() => Some(url.trim().to_string()),
        Ok(_) | Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            warn!(
                "{TEMPS_CLOUD_BACKEND_URL_VAR} is set but is not valid UTF-8; ignoring it. \
                 Unset it, or set it to the backend URL text, and restart to retry."
            );
            None
        }
    }
}

/// Consume `<TEMPS_DATA_DIR>/cloud-oidc.json` at boot, if a Cloud-hosted
/// control plane dropped one before this instance's first start (ADR-045
/// §4). The OIDC analogue of [`bootstrap_cloud_enrollment_from_env`]: a
/// one-shot first-boot *input*, never re-read to decide runtime behaviour,
/// whose result (the managed console-access `oidc_providers` row) is what
/// persists. Called synchronously and awaited, unlike the enrollment
/// bootstrap above -- this does no network I/O of its own (it is a local
/// file read plus one DB upsert), and the managed SSO provider must be in
/// place before the console is reachable, so there is nothing to gain by
/// deferring it to a background task.
///
/// A build with no Cloud plugin registered skips silently (debug log only):
/// this file only ever exists on a Cloud-hosted instance. Applying it is an
/// upsert, so it is safe to run on a boot where the instance is already
/// enrolled (re-provision, image upgrade) -- it does not depend on
/// enrollment having happened first, and it does not need
/// `cloud.console_access_enabled` to be on already: on a hosted plane the
/// file is the operator's consent, so applying it turns that switch on and
/// audits the change (ADR-045 Amendment 1). Without the file nothing here
/// touches consent.
async fn bootstrap_console_oidc_from_file(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    data_dir: &std::path::Path,
) {
    let Some(cloud_service) = service_context.get_service::<CloudService>() else {
        debug!(
            "No Cloud plugin registered on this build; skipping the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} \
             bootstrap file check"
        );
        return;
    };
    let path = data_dir.join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);
    match cloud_service.apply_console_oidc_bootstrap_file(&path).await {
        Ok(ConsoleOidcBootstrapOutcome::NotPresent) => {}
        Ok(ConsoleOidcBootstrapOutcome::Applied {
            issuer,
            client_id,
            consent,
            telemetry,
        }) => {
            report_console_oidc_bootstrap_applied(service_context, &issuer, &client_id, &consent)
                .await;
            report_telemetry_bootstrap(service_context, &telemetry).await;
        }
        Err(error) => report_console_oidc_bootstrap_error(service_context, &error).await,
    }
}

/// Log, and where consent was involved audit, a bootstrap file that failed to
/// apply. Split out of [`bootstrap_console_oidc_from_file`] so the audit
/// dispatch -- finding the registered `AuditLogger` and handing it the
/// consent trace -- can be tested against a real service context.
async fn report_console_oidc_bootstrap_error(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    error: &ConsoleOidcBootstrapError,
) {
    match error {
        ConsoleOidcBootstrapError::Parse { .. } => error!(
            %error,
            "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file exists but could not be \
             parsed; it has been left in place for inspection. Console access will start \
             without the Cloud-managed SSO provider until this is fixed and the server is \
             restarted."
        ),
        ConsoleOidcBootstrapError::Read { .. } => {
            error!(
                %error,
                "failed to apply the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file; \
                 console access will start without the Cloud-managed SSO provider until \
                 this is fixed and the server is restarted."
            )
        }
        ConsoleOidcBootstrapError::Apply { consent, .. } => {
            error!(
                %error,
                "failed to apply the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file; \
                 console access will start without the Cloud-managed SSO provider until \
                 this is fixed and the server is restarted."
            );
            // A failure that got as far as the consent write is a consent
            // event even though the apply failed: record it.
            if let ConsentTrace::WriteFailed {
                restored,
                provider_revoked,
                ..
            } = consent
            {
                error!(
                    consent_restored = *restored,
                    provider_revoked = *provider_revoked,
                    "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap tried to turn \
                     cloud.console_access_enabled on and failed; verify its state in \
                     Settings > Temps Cloud"
                );
                match service_context.get_service::<dyn temps_core::AuditLogger>() {
                    Some(audit_logger) => {
                        temps_cloud::record_console_oidc_bootstrap_failure_audit(
                            audit_logger.as_ref(),
                            CloudEnrollmentActor::UnattendedBootstrap,
                            consent,
                            &error.to_string(),
                        )
                        .await;
                    }
                    None => error!(
                        "no audit logger is registered; the \
                         CLOUD_CONSOLE_ACCESS_BOOTSTRAP_FAILED audit record was not written"
                    ),
                }
            }
        }
    }
}

/// Log and audit what the bootstrap file did to telemetry export.
async fn report_telemetry_bootstrap(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    telemetry: &temps_cloud::TelemetryConsent,
) {
    match telemetry {
        temps_cloud::TelemetryConsent::GrantedByBootstrap { .. } => warn!(
            "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file turned \
             cloud.telemetry_enabled on (on a Cloud-hosted plane the file is the operator's \
             consent); logs, traces and metrics now export to Temps Cloud. Disable it in \
             Settings > Temps Cloud if that is not intended"
        ),
        temps_cloud::TelemetryConsent::Failed { reason } => {
            error!(
                %reason,
                "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file asked for telemetry \
                 export but it could not be switched on; telemetry stays local. Turn it on \
                 in Settings > Temps Cloud"
            );
            return;
        }
        _ => return,
    }
    match service_context.get_service::<dyn temps_core::AuditLogger>() {
        Some(audit_logger) => {
            temps_cloud::record_telemetry_bootstrap_audit(
                audit_logger.as_ref(),
                CloudEnrollmentActor::UnattendedBootstrap,
                telemetry,
            )
            .await;
        }
        None => error!(
            "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file turned telemetry export on \
             but no audit logger is registered; the CLOUD_TELEMETRY_ENABLED_BY_BOOTSTRAP \
             audit record was not written"
        ),
    }
}

/// Log and audit a bootstrap file that applied. See
/// [`report_console_oidc_bootstrap_error`] for why this is separate.
async fn report_console_oidc_bootstrap_applied(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    issuer: &str,
    client_id: &str,
    consent: &ConsoleAccessConsent,
) {
    info!(
        issuer = %issuer,
        client_id = %client_id,
        console_access_enabled_by_bootstrap = consent.was_granted(),
        "applied the Cloud console-access OIDC bootstrap file; the managed SSO provider \
         is now configured"
    );
    if consent.was_granted() {
        warn!(
            "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file turned \
             cloud.console_access_enabled on (ADR-045 Amendment 1: on a Cloud-hosted \
             plane the file is the operator's consent); disable it in Settings > Temps \
             Cloud if that is not intended"
        );
    }
    match service_context.get_service::<dyn temps_core::AuditLogger>() {
        Some(audit_logger) => {
            temps_cloud::record_console_oidc_bootstrap_outcome_audit(
                audit_logger.as_ref(),
                CloudEnrollmentActor::UnattendedBootstrap,
                issuer,
                client_id,
                consent,
            )
            .await;
        }
        None => error!(
            "the {CONSOLE_OIDC_BOOTSTRAP_FILENAME} bootstrap file was applied but no audit \
             logger is registered; the CLOUD_CONSOLE_OIDC_BOOTSTRAPPED audit record \
             (and, if consent was granted, CLOUD_CONSOLE_ACCESS_ENABLED_BY_BOOTSTRAP) \
             was not written"
        ),
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
    let code = parse_cloud_enrollment_code_env(std::env::var(TEMPS_CLOUD_ENROLLMENT_CODE_VAR));
    let backend_url = parse_cloud_backend_url_env(std::env::var(TEMPS_CLOUD_BACKEND_URL_VAR));

    let Some(code) = code else {
        if backend_url.is_some() {
            info!(
                "{TEMPS_CLOUD_BACKEND_URL_VAR} is set but {TEMPS_CLOUD_ENROLLMENT_CODE_VAR} is \
                 not; ignoring it. Set both together to bootstrap unattended enrollment against \
                 a non-default Cloud backend."
            );
        }
        return None;
    };

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
        let already_linked_service = cloud_service.clone();
        let apply_backend_url_service = cloud_service.clone();
        let enroll_service = cloud_service.clone();
        let provision_service = cloud_service.clone();
        let backend_url_audit = audit_logger.clone();
        let link_audit = audit_logger.clone();
        let backup_audit = audit_logger;
        run_cloud_enrollment_bootstrap(
            &code,
            backend_url,
            move || already_linked_service.link().is_linked(),
            move |url| async move {
                apply_backend_url_service
                    .apply_bootstrap_backend_url(&url)
                    .await
            },
            move |url| async move {
                match &backend_url_audit {
                    Some(audit_logger) => {
                        temps_cloud::record_backend_url_bootstrapped_audit(
                            audit_logger.as_ref(),
                            CloudEnrollmentActor::UnattendedBootstrap,
                            &url,
                        )
                        .await;
                    }
                    None => error!(
                        "{TEMPS_CLOUD_BACKEND_URL_VAR} was applied but no audit logger is \
                         registered; the CLOUD_BACKEND_URL_BOOTSTRAPPED audit record was not \
                         written"
                    ),
                }
            },
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
                // Enrollment preserves console consent. Enable remote console
                // access explicitly from Settings > Temps Cloud or the CLI.
            },
            move || async move {
                // First-boot input supplied by whoever runs the host: the
                // instance acting on its own behalf.
                provision_service
                    .provision_managed_backups_after_enrollment(
                        temps_core::BackupScheduleAuthority::InstanceAdmin,
                    )
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
///
/// `backend_url` is the (already environment-parsed) value of
/// [`TEMPS_CLOUD_BACKEND_URL_VAR`], applied before `enroll_link` runs so
/// enrollment redeems `code` against the right tenant. Like the enrollment
/// decision itself, whether this instance is "already linked" for the
/// purpose of the backend URL is a cheap pre-check (`is_already_linked`), not
/// the atomic decision -- it exists only to avoid pointlessly calling into
/// the service on an instance that is already linked; skipping it changes
/// nothing about correctness, because `apply_backend_url`
/// ([`temps_cloud::CloudService::apply_bootstrap_backend_url`]) makes the
/// same decision again under the lock that enrollment holds, and reports it
/// as [`BootstrapBackendUrlOutcome::AlreadyLinked`] rather than writing.
///
/// The three ways applying the backend URL can end are deliberately not
/// collapsed into one log line. A rejected URL is the operator's typo and is
/// fixed by editing the variable; a write that failed is a database problem
/// and is fixed by retrying against a healthy database; an already-linked
/// instance is not a problem at all. Telling an operator with nobody to ask
/// to "fix the URL" when the URL was fine and Postgres was down sends them
/// after the wrong thing entirely.
#[allow(clippy::too_many_arguments)]
async fn run_cloud_enrollment_bootstrap<
    IL,
    A,
    ApplyBackendUrlFut,
    RB,
    BackendUrlAuditFut,
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
    backend_url: Option<String>,
    is_already_linked: IL,
    apply_backend_url: A,
    record_backend_url_audit: RB,
    enroll_link: E,
    record_link_audit: L,
    provision_backups: P,
    record_backup_audit: B,
) where
    IL: FnOnce() -> bool,
    A: FnOnce(String) -> ApplyBackendUrlFut,
    ApplyBackendUrlFut:
        std::future::Future<Output = Result<BootstrapBackendUrlOutcome, CloudServiceError>>,
    RB: FnOnce(String) -> BackendUrlAuditFut,
    BackendUrlAuditFut: std::future::Future<Output = ()>,
    E: FnOnce(String) -> EnrollFut,
    EnrollFut: std::future::Future<Output = Result<FirstLinkEnrollment, CloudServiceError>>,
    L: FnOnce() -> LinkAuditFut,
    LinkAuditFut: std::future::Future<Output = ()>,
    P: FnOnce() -> ProvisionFut,
    ProvisionFut: std::future::Future<Output = ManagedBackupOutcome>,
    B: FnOnce(ManagedBackupOutcome) -> BackupAuditFut,
    BackupAuditFut: std::future::Future<Output = ()>,
{
    if let Some(url) = backend_url {
        if is_already_linked() {
            info!(
                "{TEMPS_CLOUD_BACKEND_URL_VAR} is set but this instance is already linked to \
                 Temps Cloud; ignoring it"
            );
        } else {
            match apply_backend_url(url.clone()).await {
                Ok(BootstrapBackendUrlOutcome::Applied) => {
                    record_backend_url_audit(url.clone()).await;
                    info!(
                        backend_url = url,
                        "Applied {TEMPS_CLOUD_BACKEND_URL_VAR} bootstrap input; unattended \
                         enrollment will target this backend"
                    );
                }
                Ok(BootstrapBackendUrlOutcome::AlreadyLinked) => {
                    // An operator linked this instance while the bootstrap was
                    // starting up. Their backend stands, nothing was written,
                    // and the enrollment below will reach the same conclusion
                    // atomically -- so there is nothing to audit and no reason
                    // to stop here.
                    info!(
                        "{TEMPS_CLOUD_BACKEND_URL_VAR} was not applied: this instance was linked \
                         to Temps Cloud before the bootstrap could write it, and the established \
                         link's backend stands. Change it from Settings > Cloud if it is wrong."
                    );
                }
                Err(CloudServiceError::InvalidBackend { reason }) => {
                    warn!(
                        %reason,
                        "{TEMPS_CLOUD_BACKEND_URL_VAR} is not a usable Temps Cloud backend URL; \
                         skipping unattended Temps Cloud enrollment rather than enrolling \
                         against the default backend. Fix the URL and restart to retry, or \
                         connect from Settings > Cloud."
                    );
                    return;
                }
                Err(error) => {
                    warn!(
                        %error,
                        backend_url = url,
                        "{TEMPS_CLOUD_BACKEND_URL_VAR} is a valid URL but could not be saved to \
                         this instance's settings, so unattended Temps Cloud enrollment was \
                         skipped rather than run against the default backend. The URL is not \
                         the problem -- check that the database is reachable and healthy, then \
                         restart to retry, or connect from Settings > Cloud once it is."
                    );
                    return;
                }
            }
        }
    }

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
/// `GET /_temps/gate/authorize` (ADR 065 D8). Without a console session
/// (signed out, or MFA not finished) this is the console itself: its
/// sign-in page remembers the URL and comes back here once signed in.
async fn gate_authorize(
    state: Arc<temps_ingress::reachability::gate_handlers::GateConsoleState>,
    req: Request,
) -> Response {
    let Some(auth) = req.extensions().get::<temps_auth::AuthContext>().cloned() else {
        return serve_static_file(req).await;
    };
    let metadata = req
        .extensions()
        .get::<temps_core::RequestMetadata>()
        .cloned();
    temps_ingress::reachability::gate_handlers::authorize_response(
        &state,
        &auth,
        metadata.as_ref(),
        req.uri().query(),
    )
    .await
}

async fn serve_static_file(req: Request) -> Response {
    with_console_security_headers(serve_static_from(website(), req))
}

/// Same, but always the ORIGINAL console bundle (for the platform-console
/// listener when the root bundle has been overridden by an embedding binary).
async fn serve_original_console(req: Request) -> Response {
    with_console_security_headers(serve_static_from(&WEBSITE, req))
}

/// Security headers every console UI (SPA fallback) response carries,
/// independent of the per-project `SecurityHeadersSettings` (which govern
/// *deployed apps* behind the proxy and default to off).
///
/// Scoped to the SPA/static fallback on purpose, not to `/api`: the console
/// frames plugin UIs served same-origin from `/api/x/{plugin}/ui/`, so a
/// blanket `X-Frame-Options: DENY` on the API would break them.
///
/// The CSP is deliberately non-restrictive about script/style/connect/img
/// sources so the SPA keeps working unchanged; it only forbids framing of the
/// console (clickjacking), `<base>` hijacking, and plugin content.
///
/// HSTS is intentionally not sent here: whether the console is reached over
/// TLS is only known from `X-Forwarded-Proto`, which a client can influence,
/// and pinning HTTPS on a plain-HTTP install would lock operators out.
const CONSOLE_SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    (
        "content-security-policy",
        "frame-ancestors 'none'; base-uri 'self'; object-src 'none'",
    ),
];

/// Add [`CONSOLE_SECURITY_HEADERS`] to `response`, never overriding a value
/// the handler already set.
fn with_console_security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in CONSOLE_SECURITY_HEADERS {
        let name = header::HeaderName::from_static(name);
        if !headers.contains_key(&name) {
            headers.insert(name, header::HeaderValue::from_static(value));
        }
    }
    response
}

/// Detail text for a request path that must never fall through to the SPA,
/// or `None` when the console may serve it.
///
/// The returned text is a fixed literal (no request data), so it is safe to
/// embed in the JSON body verbatim.
fn unmatched_api_detail(path: &str) -> Option<&'static str> {
    if path.starts_with("/api/") {
        Some("No API route matched")
    } else if path == "/ai/v1" || path.starts_with("/ai/v1/") {
        Some(
            "No API route matched. The AI gateway is served under /api/ai/v1, \
             for example /api/ai/v1/chat/completions",
        )
    } else {
        None
    }
}

fn serve_static_from(site: &'static Dir<'static>, req: Request) -> Response {
    let raw_path = req.uri().path();

    // Never serve the SPA for /api/ paths — those should 404 if unmatched
    // by any API router (including external plugin proxies). The same goes for
    // `/ai/v1/*`: the AI gateway lives under `/api/ai/v1`, and answering a
    // client that dropped the `/api` prefix with the console HTML (200) makes
    // every OpenAI SDK fail with an unparsable body instead of a pointer to
    // the right URL.
    if let Some(detail) = unmatched_api_detail(raw_path) {
        let body = format!(r#"{{"status":404,"title":"Not Found","detail":"{detail}"}}"#);
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(body),
        )
            .into_response();
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

            // `mime_guess` only yields valid header values; if one ever were
            // not, axum answers 500 instead of panicking the worker.
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime_type),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
                Body::from(file.contents()),
            )
                .into_response()
        }
        None => {
            // If file not found, try serving index.html (for SPA routing)
            if let Some(index) = site.get_file("index.html") {
                debug!("File not found, serving index.html for SPA routing");
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "text/html"),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    Body::from(index.contents()),
                )
                    .into_response()
            } else {
                debug!("File not found and no index.html available: {}", path);
                (StatusCode::NOT_FOUND, Body::from("404 Not Found")).into_response()
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

/// Make sure the optional GeoLite2-ASN database is present, downloading the
/// copy committed to the repository (or MaxMind's, with a license key) into
/// the data directory when neither the working directory nor the data
/// directory has one. Docker images already ship it; this covers binary
/// installs.
///
/// Never fails startup: without the ASN database, datacenter detection is off
/// and the analytics settings page says so, but everything else works.
async fn ensure_geolite2_asn_database(default_db_path: &Path, license_key: Option<&str>) {
    let cwd_path = PathBuf::from(format!("./{}", temps_geo::ASN_DB_FILENAME));
    if cwd_path.exists() || default_db_path.exists() {
        debug!("✓ GeoLite2-ASN database present");
        return;
    }

    let http = match temps_geo::refresh::build_http_client() {
        Ok(http) => http,
        Err(e) => {
            warn!(
                "Could not build the GeoLite2-ASN download client ({}); datacenter traffic \
                 detection stays off until {} is provided",
                e,
                default_db_path.display()
            );
            return;
        }
    };

    info!(
        "Downloading {} to {} (MaxMind license key configured: {})",
        temps_geo::ASN_DB_FILENAME,
        default_db_path.display(),
        license_key.is_some()
    );
    match temps_geo::ensure_mmdb_present_for(
        temps_geo::GeoDatabaseKind::Asn,
        default_db_path,
        license_key,
        &http,
    )
    .await
    {
        Ok(_) => debug!("✓ GeoLite2-ASN database downloaded"),
        Err(e) => warn!(
            "Could not download {} to {}: {}. Datacenter traffic detection stays off until \
             the file is provided; geolocation is unaffected.",
            temps_geo::ASN_DB_FILENAME,
            default_db_path.display(),
            temps_geo::redact_license_key(e.to_string(), license_key)
        ),
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
        Err(e) => Err(anyhow::Error::new(
            super::startup_failure::ConsoleStartupError::GeoDatabase {
                checked: search_paths.clone(),
                reason: e.to_string(),
                message: format!(
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
                ),
            },
        )),
    }
}

/// Parameters for starting the console API server.
///
/// Groups the dependencies needed by [`start_console_api`] to keep the
/// function signature under clippy's argument limit.
pub struct ConsoleApiParams {
    /// Set when this start upgrades an existing installation (new version
    /// since the last start, or migrations applied to an existing database);
    /// reported as `upgrade_completed` alongside `instance_started`.
    pub upgrade_probe: Option<super::upgrade_telemetry::UpgradeProbe>,
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
    /// The process's activator (ADR-055 phase 10), shared with the
    /// on-demand manager that registers environments with it.
    pub activator: Arc<temps_workload_activator::Activator>,
    /// Extra control-plane runtime drivers for the workload plane. Empty
    /// today.
    pub local_drivers: Arc<temps_workload_plane::LocalDrivers>,
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
    /// Resolver address published by the proxy-owned DNS listener. The
    /// deployer reads this slot for each new container and never assumes that
    /// an enabled setting means a listener actually started.
    pub overlay_dns_slot: temps_dns::OverlayDnsSlot,
    /// Node 0's network policy view. `NetworkPolicyPlugin`'s enforcer
    /// stores a snapshot after each pass; the proxy-owned resolver answers
    /// restricted workloads from the same slot.
    pub dns_policy_slot: temps_dns::DnsPolicySlot,
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
    /// Shared with the in-process proxy. Marked running here when the
    /// listeners start; the caller records the failure if this function
    /// returns an error, so the proxy can explain it on the console URL.
    pub startup_state: Arc<temps_core::console_startup::ConsoleStartupState>,
}

/// Bind a console listener, naming the address in the error so the startup
/// status page can tell the operator which address is taken.
async fn bind_console_listener(
    address: &str,
) -> Result<TcpListener, super::startup_failure::ConsoleStartupError> {
    TcpListener::bind(address).await.map_err(|source| {
        super::startup_failure::ConsoleStartupError::ListenerBind {
            address: address.to_string(),
            source,
        }
    })
}

/// Storage backend selection error for the log aggregator. Owned by
/// `temps-file-store`, the single reader of the `TEMPS_LOG_S3_*` connection.
pub use temps_file_store::s3_config::LogStorageConfigError;

/// Resolve the log-aggregator storage backend.
///
/// Delegates to `temps_file_store::s3_config::resolve_log_storage_backend_for`
/// so the `TEMPS_LOG_STORAGE_BACKEND` / `TEMPS_LOG_S3_*` connection has one
/// reader shared with static-site/CAS storage and stateless startup, instead
/// of two copies that could drift. A missing variable is a typed error the
/// caller renders, never a panic.
fn log_aggregator_storage_config(
    data_dir: &std::path::Path,
    stateless_instance_id: Option<&str>,
) -> Result<StorageConfig, LogStorageConfigError> {
    use temps_file_store::s3_config::{
        resolve_log_storage_backend_for, resolve_stateless_storage_for, LogStorageBackend,
    };

    let stateless = resolve_stateless_storage_for(stateless_instance_id)?;
    Ok(match resolve_log_storage_backend_for(&stateless)? {
        LogStorageBackend::Filesystem => StorageConfig::Filesystem {
            base_path: data_dir.join("log-aggregator"),
        },
        LogStorageBackend::S3(storage) => StorageConfig::S3 {
            bucket: storage.bucket,
            region: storage.region,
            endpoint: storage.endpoint,
            access_key_id: storage.access_key_id,
            secret_access_key: storage.secret_access_key,
            prefix: storage.prefix,
            force_path_style: storage.force_path_style,
        },
    })
}

/// Build a ClickHouse-backed metrics store from the server config, or `None`
/// when ClickHouse is not configured.
///
/// Returns `Some(store)` only when all four `TEMPS_CLICKHOUSE_*` vars are set
/// (`config.is_clickhouse_enabled()`). When the monitoring store is set to
/// ClickHouse but the env vars are absent, this logs a warning and returns
/// `None` so the caller falls back to TimescaleDB (fail-open to the default
/// path, never silently losing metrics). Construction does no I/O.
///
/// `compression` is the wire-compression mode every ClickHouse client of this
/// process uses (`ConfigService::clickhouse_compression`). Migrations are
/// applied on a detached task and retried until they apply, so a server that
/// is still starting — or one whose responses cannot be decoded until the
/// operator fixes it — does not leave the store without its tables until the
/// next restart.
fn build_ch_metrics_store(
    config: &ServerConfig,
    compression: temps_clickhouse::ClickHouseCompression,
) -> Option<Arc<dyn temps_metrics::MetricsStore>> {
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
    )
    .with_compression(compression);
    let store = Arc::new(ClickhouseMetricsStore::new(cfg));

    // Run migrations in the background so startup is not blocked, retrying
    // until they apply. `spawn_retrying` works with or without a Tokio runtime
    // on this thread.
    let client = store.client().clone();
    let database = config.clickhouse_database.clone().unwrap_or_default();
    temps_clickhouse::spawn_retrying(
        "resource-metrics",
        temps_clickhouse::MigrationRetryPolicy::default(),
        move || {
            let client = client.clone();
            let database = database.clone();
            async move {
                temps_metrics::clickhouse_migrations::apply_migrations(&client, &database).await
            }
        },
        |report| {
            debug!(
                applied = ?report.applied,
                skipped = report.skipped.len(),
                "ClickHouse resource-metrics migrations applied"
            )
        },
    );

    // Every producer (scraper, samplers, health monitors, OTLP) writes a few
    // dozen points per tick; buffering turns those into a few large inserts
    // instead of one ClickHouse part each.
    Some(temps_metrics::BufferedMetricsStore::wrap(
        store as Arc<dyn MetricsStore>,
        temps_metrics::BufferConfig::default(),
    ))
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

/// Test-only: every API operation must be exposed to the assistant or withheld
/// with a reason, so a new endpoint cannot be invisible by accident.
#[cfg(test)]
mod ai_tool_policy;

/// Test-only: every request field that takes a credential must be flagged in
/// the spec, so the assistant can take it through a secret slot.
#[cfg(test)]
mod ai_secret_fields;

mod startup;
pub use startup::start_console_api;

/// Test-only: the allowlists in `temps_ai_api_tools::allowlists` checked
/// against the real API documents this binary serves.
#[cfg(test)]
mod ai_tool_allowlist_tests;

/// The console's public surface and nothing else (ADR-051 D4).
///
/// Serves the health probes and the public router under `/api`. It has no
/// admin routes, no MCP endpoint and no dashboard fallback, so a request that
/// reaches it cannot execute an admin handler or receive the SPA whatever its
/// path, `Host`, forwarding headers or cookies: the boundary is what is
/// mounted here, not a path match. The proxy sends it the public ingress
/// hostname and every request the admin gate denies.
///
/// Handlers on `public_router` authenticate the request (a provider
/// signature, a capability token in the path, a scoped API key), never the
/// session — they have to work for callers that hold no cookie at all.
fn public_only_app(health: Router, public_router: Router) -> Router {
    Router::new()
        .merge(health)
        .nest("/api", public_router)
        .fallback(public_only_not_found)
        .layer(axum::middleware::from_fn(track_server_errors))
}

/// Serve the public-only app until shutdown. Resolves immediately when this
/// process has no such listener (a split console started without
/// `--console-ingress-address`), so callers can join it unconditionally.
async fn serve_public_only(
    ingress: Option<(TcpListener, Router)>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    match ingress {
        Some((listener, app)) => {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown)
            .await
        }
        None => Ok(()),
    }
}

/// 404 for everything that is not a public route. A browser gets the same
/// branded page the proxy renders for an unknown host, so a non-allowlisted
/// visitor sees exactly what they saw before this listener existed; an API
/// client gets an empty body.
async fn public_only_not_found(req: Request) -> Response {
    use axum::response::IntoResponse;

    let headers = req.headers();
    let wants_html = !req.uri().path().starts_with("/api/")
        && headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"));
    if !wants_html {
        return (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")]).into_response();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    (
        StatusCode::NOT_FOUND,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        ],
        temps_proxy::branded_404::render(host, request_id),
    )
        .into_response()
}

#[cfg(test)]
mod public_only_app_tests {
    //! ADR-051 invariant 1: a request that reaches the public-only listener
    //! cannot execute an admin handler or receive the dashboard, whatever its
    //! path or headers. The boundary is what `public_only_app` mounts.
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use std::sync::atomic::AtomicBool;
    use tower::ServiceExt; // for `oneshot`

    /// A public router shaped like the real one: receivers only.
    fn public_router() -> Router {
        Router::new()
            .route("/emails/{email_id}/track/open", get(|| async { "pixel" }))
            .route("/webhook/git/gitlab/events", post(|| async { "ok" }))
            .route("/_temps/event", post(|| async { "ok" }))
    }

    /// Routes that exist on the admin router and on the merged console
    /// listener, and must not exist here.
    fn admin_router() -> Router {
        Router::new()
            .route("/settings", get(|| async { "settings" }))
            .route("/projects", get(|| async { "projects" }))
            .route("/_temps/internal", get(|| async { "internal" }))
    }

    fn app() -> Router {
        public_only_app(
            health_router(Arc::new(AtomicBool::new(true))),
            public_router(),
        )
    }

    async fn send(app: Router, request: Request<Body>) -> Response {
        app.oneshot(request).await.expect("router responds")
    }

    fn request(method: &str, path: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "hooks.example.com")
            .body(Body::empty())
            .expect("request builds")
    }

    #[tokio::test]
    async fn serves_public_receivers_and_health_probes() {
        for (method, path) in [
            ("GET", "/healthz"),
            ("GET", "/readyz"),
            (
                "GET",
                "/api/emails/0b5a51a2-2b0a-4c0b-9f0e-6a1f0d1c2e3f/track/open",
            ),
            ("POST", "/api/webhook/git/gitlab/events"),
            ("POST", "/api/_temps/event"),
        ] {
            assert_eq!(
                send(app(), request(method, path)).await.status(),
                StatusCode::OK,
                "{method} {path} must be served on the public-only listener"
            );
        }
    }

    #[tokio::test]
    async fn admin_routes_and_the_dashboard_are_not_mounted() {
        // Sanity check: these paths do answer on a console that mounts them,
        // so a 404 below is the boundary and not a typo in the path.
        let merged = Router::new()
            .nest("/api", public_router().merge(admin_router()))
            .fallback(|| async { "dashboard" });
        for path in [
            "/",
            "/api/settings",
            "/api/projects",
            "/api/_temps/internal",
        ] {
            assert_eq!(
                send(merged.clone(), request("GET", path)).await.status(),
                StatusCode::OK
            );
        }

        for path in [
            "/",
            "/dashboard",
            "/index.html",
            "/settings",
            "/api/settings",
            "/api/projects",
            // An authenticated sibling of a public ingest route.
            "/api/_temps/internal",
            "/mcp",
            "/mcp/tools",
            "/api/swagger-ui",
        ] {
            let response = send(app(), request("GET", path)).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} must not be reachable on the public-only listener"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("no-store"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn headers_cannot_steer_a_request_onto_an_admin_route() {
        let request = Request::builder()
            .uri("/api/settings")
            .header(header::HOST, "console.example.com")
            .header("x-forwarded-host", "console.example.com")
            .header("x-forwarded-for", "127.0.0.1")
            .header(header::COOKIE, "session=valid-admin-session")
            .header(header::AUTHORIZATION, "Bearer tk_admin")
            .body(Body::empty())
            .expect("request builds");
        assert_eq!(send(app(), request).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_browser_gets_the_branded_404_and_an_api_client_an_empty_one() {
        let browser = Request::builder()
            .uri("/dashboard")
            .header(header::HOST, "hooks.example.com")
            .header(header::ACCEPT, "text/html,application/xhtml+xml")
            .body(Body::empty())
            .expect("request builds");
        let response = send(app(), browser).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(
            html.contains("<!doctype html>"),
            "expected the branded page"
        );
        assert!(html.contains("hooks.example.com"));
        assert!(
            !html.contains("id=\"root\""),
            "the dashboard bundle must never be served here"
        );

        // `/api/*` never gets HTML, even from a browser.
        let api = Request::builder()
            .uri("/api/settings")
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .expect("request builds");
        let response = send(app(), api).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(body.is_empty());
    }
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
        already_linked: AtomicBool,
        backend_url_applied: std::sync::Mutex<Option<String>>,
        backend_url_audited: std::sync::Mutex<Option<String>>,
        enroll_called: AtomicBool,
        link_audited: AtomicBool,
        provision_called: AtomicBool,
        backup_audited: std::sync::Mutex<Option<ManagedBackupOutcome>>,
    }

    impl BootstrapProbe {
        fn backend_url_applied(&self) -> Option<String> {
            self.backend_url_applied
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn backend_url_audited(&self) -> Option<String> {
            self.backend_url_audited
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

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
    ///
    /// `backend_url` and `apply_backend_url` cover the
    /// `TEMPS_CLOUD_BACKEND_URL` bootstrap step; most callers pass `None` and
    /// a no-op `apply_backend_url` since they only exercise the enrollment
    /// half. `probe.already_linked` (set before calling, defaults to
    /// `false`) drives the cheap pre-check that skips applying the backend
    /// URL on an already-linked instance.
    async fn drive_bootstrap<EnrollFut, ProvisionFut, ApplyBackendUrlFut>(
        probe: &Arc<BootstrapProbe>,
        backend_url: Option<&str>,
        apply_backend_url: impl FnOnce(String) -> ApplyBackendUrlFut,
        enroll: impl FnOnce() -> EnrollFut,
        provision: impl FnOnce() -> ProvisionFut,
    ) where
        EnrollFut: std::future::Future<Output = EnrollResult>,
        ProvisionFut: std::future::Future<Output = ManagedBackupOutcome>,
        ApplyBackendUrlFut:
            std::future::Future<Output = Result<BootstrapBackendUrlOutcome, CloudServiceError>>,
    {
        let already_linked_probe = probe.clone();
        let apply_backend_url_probe = probe.clone();
        let backend_url_audit_probe = probe.clone();
        let enroll_probe = probe.clone();
        let link_probe = probe.clone();
        let provision_probe = probe.clone();
        let backup_probe = probe.clone();
        let enroll_fut = enroll();
        let provision_fut = provision();
        run_cloud_enrollment_bootstrap(
            "the-code",
            backend_url.map(str::to_string),
            move || already_linked_probe.already_linked.load(Ordering::SeqCst),
            move |url| async move {
                *apply_backend_url_probe
                    .backend_url_applied
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(url.clone());
                apply_backend_url(url).await
            },
            move |url| async move {
                *backend_url_audit_probe
                    .backend_url_audited
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(url);
            },
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

    /// [`drive_bootstrap`] for the common case: no `TEMPS_CLOUD_BACKEND_URL`
    /// in play, so the enrollment steps are all that matter.
    async fn drive_enrollment_only<EnrollFut, ProvisionFut>(
        probe: &Arc<BootstrapProbe>,
        enroll: impl FnOnce() -> EnrollFut,
        provision: impl FnOnce() -> ProvisionFut,
    ) where
        EnrollFut: std::future::Future<Output = EnrollResult>,
        ProvisionFut: std::future::Future<Output = ManagedBackupOutcome>,
    {
        drive_bootstrap(
            probe,
            None,
            |_url| async { Ok(BootstrapBackendUrlOutcome::Applied) },
            enroll,
            provision,
        )
        .await;
    }

    #[tokio::test]
    async fn unattended_cloud_enrollment_succeeds_with_a_valid_code() {
        let probe = Arc::new(BootstrapProbe::default());
        drive_enrollment_only(
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
        drive_enrollment_only(
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
        drive_enrollment_only(
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
        drive_enrollment_only(
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
        let bootstrap =
            drive_enrollment_only(&probe, std::future::pending::<EnrollResult>, || async {
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
        let bootstrap = drive_enrollment_only(
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

    #[tokio::test]
    async fn backend_url_bootstrap_is_applied_audited_and_then_enrolled_against_it() {
        // The headline flow: a provisioning tool sets both variables together
        // on a fresh instance, pointing it at a non-default Cloud backend
        // before the enrollment code is redeemed.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            Some("https://cloud.staging.example"),
            |_url| async { Ok(BootstrapBackendUrlOutcome::Applied) },
            || async { established() },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert_eq!(
            probe.backend_url_applied().as_deref(),
            Some("https://cloud.staging.example"),
            "the backend URL must be persisted before enrollment runs"
        );
        assert_eq!(
            probe.backend_url_audited().as_deref(),
            Some("https://cloud.staging.example"),
            "a persisted backend URL must be audited"
        );
        assert!(
            probe.enroll_called.load(Ordering::SeqCst),
            "enrollment must run once the backend URL is applied"
        );
        assert!(probe.link_audited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn backend_url_bootstrap_invalid_url_skips_enrollment_entirely() {
        // An operator's typo, or a code minted for one Cloud but the URL for
        // another, must never fall back to enrolling against the default
        // backend -- that would silently link the wrong tenant.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            Some("not a valid url"),
            |_url| async {
                Err(CloudServiceError::InvalidBackend {
                    reason: "relative URL without a base".to_string(),
                })
            },
            || async { established() },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(
            probe.backend_url_applied().is_some(),
            "applying the URL must still be attempted so the failure reason is known"
        );
        assert!(
            probe.backend_url_audited().is_none(),
            "an invalid URL must never be audited as applied"
        );
        assert!(
            !probe.enroll_called.load(Ordering::SeqCst),
            "enrollment must not run against the default backend when the configured one is invalid"
        );
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn backend_url_bootstrap_that_could_not_be_saved_says_so_instead_of_blaming_the_url() {
        // A transient database failure while persisting cloud.backend_url is
        // not the operator's typo. Enrollment is still skipped -- running it
        // would redeem the code against the default backend, exactly what
        // this variable exists to prevent -- but the reason reported must be
        // the failed write, so the operator retries against the database
        // instead of hunting a URL that was already accepted.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            Some("https://cloud.staging.example"),
            |_url| async {
                Err(CloudServiceError::Configuration(
                    temps_config::ConfigServiceError::Database(DbErr::Custom(
                        "connection closed".to_string(),
                    )),
                ))
            },
            || async { established() },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(
            probe.backend_url_applied().is_some(),
            "the write must be attempted so its failure is the reported reason"
        );
        assert!(
            probe.backend_url_audited().is_none(),
            "a backend URL that was never persisted must never be audited as applied"
        );
        assert!(
            !probe.enroll_called.load(Ordering::SeqCst),
            "a failed write leaves settings on the default backend; enrolling there is the \
             wrong tenant"
        );
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn backend_url_bootstrap_lost_to_a_link_established_mid_flight_is_a_no_op_not_a_failure()
    {
        // The race this bootstrap's atomicity exists for, seen from the CLI:
        // the pre-check said unlinked, and by the time the service took the
        // enrollment lock an operator's enrollment had landed. The service
        // reports AlreadyLinked instead of writing, and the bootstrap must
        // treat that as a normal outcome -- nothing persisted, so nothing
        // audited, and enrollment still runs to reach the same conclusion
        // atomically rather than aborting on a non-problem.
        let probe = Arc::new(BootstrapProbe::default());
        drive_bootstrap(
            &probe,
            Some("https://cloud.staging.example"),
            |_url| async { Ok(BootstrapBackendUrlOutcome::AlreadyLinked) },
            || async { Ok(FirstLinkEnrollment::AlreadyLinked) },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(
            probe.backend_url_applied().is_some(),
            "the service must be the one to decide, so it must be called"
        );
        assert!(
            probe.backend_url_audited().is_none(),
            "nothing was written, so there is nothing to audit"
        );
        assert!(
            probe.enroll_called.load(Ordering::SeqCst),
            "the enrollment step still runs and makes its own atomic decision"
        );
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn backend_url_bootstrap_is_ignored_on_an_already_linked_instance() {
        // The variable exists to point a *fresh* instance at the right
        // tenant. An instance that is already linked must not have its
        // backend URL rewritten by a leftover value from a previous
        // provisioning run -- enrollment still proceeds and makes its own
        // (atomic) already-linked decision independently.
        let probe = Arc::new(BootstrapProbe::default());
        probe.already_linked.store(true, Ordering::SeqCst);
        drive_bootstrap(
            &probe,
            Some("https://cloud.staging.example"),
            |_url| async { Ok(BootstrapBackendUrlOutcome::Applied) },
            || async { Ok(FirstLinkEnrollment::AlreadyLinked) },
            || async { ManagedBackupOutcome::Provisioned },
        )
        .await;

        assert!(
            probe.backend_url_applied().is_none(),
            "an already-linked instance's backend URL must not be rewritten"
        );
        assert!(probe.backend_url_audited().is_none());
        assert!(probe.enroll_called.load(Ordering::SeqCst));
        assert!(!probe.link_audited.load(Ordering::SeqCst));
        assert!(!probe.provision_called.load(Ordering::SeqCst));
    }

    #[test]
    fn cloud_backend_url_env_is_absent_when_the_variable_is_unset() {
        assert_eq!(
            parse_cloud_backend_url_env(Err(std::env::VarError::NotPresent)),
            None
        );
    }

    #[test]
    fn cloud_backend_url_env_is_absent_when_blank() {
        assert_eq!(parse_cloud_backend_url_env(Ok("".to_string())), None);
        assert_eq!(parse_cloud_backend_url_env(Ok("   ".to_string())), None);
    }

    #[test]
    fn cloud_backend_url_env_degrades_to_absent_on_non_unicode_value() {
        assert_eq!(
            parse_cloud_backend_url_env(Err(std::env::VarError::NotUnicode(
                std::ffi::OsString::from("invalid-value")
            ))),
            None
        );
    }

    #[test]
    fn cloud_backend_url_env_trims_and_is_present_when_set() {
        assert_eq!(
            parse_cloud_backend_url_env(Ok("  https://cloud.staging.example  ".to_string())),
            Some("https://cloud.staging.example".to_string())
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
mod heartbeat_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use temps_core::telemetry::{TelemetryEvent, TelemetryReporter};

    /// Reporter whose enabled state a test can flip, recording what it was
    /// asked to send.
    #[derive(Default)]
    struct SwitchableReporter {
        enabled: AtomicBool,
        sent: Mutex<Vec<String>>,
    }

    impl TelemetryReporter for SwitchableReporter {
        fn report(&self, event: TelemetryEvent) {
            self.sent.lock().unwrap().push(event.event_type);
        }

        fn is_enabled(&self) -> bool {
            self.enabled.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn heartbeat_is_not_built_or_sent_while_telemetry_is_disabled() {
        let reporter = SwitchableReporter::default();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        assert!(!emit_heartbeat(&reporter, &db).await);
        assert!(reporter.sent.lock().unwrap().is_empty());
        // Nothing was queried either: the instance counts are only gathered
        // for an event that will actually be sent.
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn heartbeat_follows_a_runtime_change_without_restart() {
        let reporter = SwitchableReporter::default();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        reporter.enabled.store(true, Ordering::SeqCst);
        assert!(emit_heartbeat(&reporter, &db).await);
        reporter.enabled.store(false, Ordering::SeqCst);
        assert!(!emit_heartbeat(&reporter, &db).await);

        assert_eq!(
            *reporter.sent.lock().unwrap(),
            vec!["instance_heartbeat".to_string()]
        );
    }
}

#[cfg(test)]
mod console_oidc_bootstrap_audit_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use temps_cloud::ConsoleOidcBootstrapParseError;
    use temps_cloud::{
        CLOUD_CONSOLE_ACCESS_BOOTSTRAP_FAILED, CLOUD_CONSOLE_ACCESS_ENABLED_BY_BOOTSTRAP,
        CLOUD_CONSOLE_OIDC_BOOTSTRAPPED,
    };
    use temps_cloud_client::CloudFeatureSwitches;
    use temps_core::plugin::ServiceRegistrationContext;
    use temps_core::{AuditLogger, AuditOperation};

    /// Records the operation type of every audit entry it is handed.
    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.operations
                .lock()
                .unwrap()
                .push(operation.operation_type());
            Ok(())
        }
    }

    impl RecordingAuditLogger {
        fn recorded(&self) -> Vec<String> {
            self.operations.lock().unwrap().clone()
        }
    }

    fn context_with_logger() -> (ServiceRegistrationContext, Arc<RecordingAuditLogger>) {
        let recorder = Arc::new(RecordingAuditLogger::default());
        let context = ServiceRegistrationContext::new();
        context.register_service::<dyn AuditLogger>(recorder.clone());
        (context, recorder)
    }

    fn path() -> PathBuf {
        PathBuf::from("/data/cloud-oidc.json")
    }

    fn apply_error(consent: ConsentTrace) -> ConsoleOidcBootstrapError {
        ConsoleOidcBootstrapError::Apply {
            path: path(),
            reason: "the settings write failed".to_string(),
            consent,
        }
    }

    fn failed_consent_write() -> ConsentTrace {
        ConsentTrace::WriteFailed {
            previous: CloudFeatureSwitches::default(),
            restored: true,
            provider_revoked: true,
        }
    }

    fn granted() -> ConsoleAccessConsent {
        ConsoleAccessConsent::GrantedByBootstrap {
            previous: CloudFeatureSwitches::default(),
            new: CloudFeatureSwitches {
                console_access: true,
                ..CloudFeatureSwitches::default()
            },
        }
    }

    #[tokio::test]
    async fn a_failed_consent_write_is_audited_through_the_registered_logger() {
        let (context, recorder) = context_with_logger();

        report_console_oidc_bootstrap_error(&context, &apply_error(failed_consent_write())).await;

        assert_eq!(
            recorder.recorded(),
            vec![CLOUD_CONSOLE_ACCESS_BOOTSTRAP_FAILED.to_string()]
        );
    }

    #[tokio::test]
    async fn an_apply_failure_before_the_consent_write_is_not_audited() {
        let (context, recorder) = context_with_logger();

        report_console_oidc_bootstrap_error(&context, &apply_error(ConsentTrace::Untouched)).await;

        assert!(recorder.recorded().is_empty());
    }

    #[tokio::test]
    async fn unreadable_and_unparsable_files_are_not_audited() {
        let (context, recorder) = context_with_logger();

        report_console_oidc_bootstrap_error(
            &context,
            &ConsoleOidcBootstrapError::Read {
                path: path(),
                reason: "permission denied".to_string(),
            },
        )
        .await;
        report_console_oidc_bootstrap_error(
            &context,
            &ConsoleOidcBootstrapError::Parse {
                path: path(),
                source: ConsoleOidcBootstrapParseError::EmptyField { field: "issuer" },
            },
        )
        .await;

        assert!(recorder.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_failed_consent_write_without_a_registered_logger_does_not_panic() {
        let context = ServiceRegistrationContext::new();

        report_console_oidc_bootstrap_error(&context, &apply_error(failed_consent_write())).await;
        report_console_oidc_bootstrap_applied(
            &context,
            "https://issuer.example",
            "client",
            &granted(),
        )
        .await;

        assert!(context.get_service::<dyn AuditLogger>().is_none());
    }

    #[tokio::test]
    async fn an_applied_bootstrap_that_granted_consent_records_both_events() {
        let (context, recorder) = context_with_logger();

        report_console_oidc_bootstrap_applied(
            &context,
            "https://issuer.example",
            "client",
            &granted(),
        )
        .await;

        assert_eq!(
            recorder.recorded(),
            vec![
                CLOUD_CONSOLE_OIDC_BOOTSTRAPPED.to_string(),
                CLOUD_CONSOLE_ACCESS_ENABLED_BY_BOOTSTRAP.to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn an_applied_bootstrap_that_found_consent_already_on_records_only_the_apply() {
        let (context, recorder) = context_with_logger();

        report_console_oidc_bootstrap_applied(
            &context,
            "https://issuer.example",
            "client",
            &ConsoleAccessConsent::AlreadyEnabled,
        )
        .await;

        assert_eq!(
            recorder.recorded(),
            vec![CLOUD_CONSOLE_OIDC_BOOTSTRAPPED.to_string()]
        );
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

        let config = log_aggregator_storage_config(std::path::Path::new("/srv/temps"), None)
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

        let error = log_aggregator_storage_config(std::path::Path::new("/srv/temps"), None)
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

        let error = log_aggregator_storage_config(std::path::Path::new("/srv/temps"), None)
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

        let config = log_aggregator_storage_config(std::path::Path::new("/srv/temps"), None)
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

#[cfg(test)]
mod spa_fallback_tests {
    use super::*;

    #[test]
    fn api_paths_never_fall_through_to_the_spa() {
        assert_eq!(
            unmatched_api_detail("/api/does-not-exist"),
            Some("No API route matched")
        );
    }

    #[test]
    fn ai_gateway_path_without_api_prefix_points_to_the_real_base_url() {
        for path in ["/ai/v1", "/ai/v1/models", "/ai/v1/chat/completions"] {
            let detail = unmatched_api_detail(path)
                .unwrap_or_else(|| panic!("{path} must not be answered with the console HTML"));
            assert!(
                detail.contains("/api/ai/v1"),
                "detail for {path} must name the correct base URL, got: {detail}"
            );
        }
    }

    #[test]
    fn console_routes_are_still_served_by_the_spa() {
        for path in ["/", "/ai", "/ai-first", "/ai/v1x", "/projects/1/ai/v1"] {
            assert_eq!(
                unmatched_api_detail(path),
                None,
                "{path} is a console route and must keep the SPA fallback"
            );
        }
    }

    /// F-002: console UI responses carry anti-clickjacking / nosniff /
    /// referrer / CSP headers regardless of per-project header settings.
    #[tokio::test]
    async fn console_spa_responses_carry_security_headers() {
        for path in ["/", "/projects/1", "/does/not/exist.js"] {
            let request = axum::http::Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request builds");
            let response = serve_original_console(request).await;
            let get = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            assert_eq!(get("x-frame-options").as_deref(), Some("DENY"), "{path}");
            assert_eq!(
                get("x-content-type-options").as_deref(),
                Some("nosniff"),
                "{path}"
            );
            assert_eq!(
                get("referrer-policy").as_deref(),
                Some("strict-origin-when-cross-origin"),
                "{path}"
            );
            let csp = get("content-security-policy").unwrap_or_default();
            assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
            // Must not restrict what the SPA itself loads.
            assert!(!csp.contains("script-src"), "{path}: {csp}");
            assert!(!csp.contains("default-src"), "{path}: {csp}");
            assert!(response
                .headers()
                .get("strict-transport-security")
                .is_none());
        }
    }

    #[test]
    fn console_security_headers_never_override_handler_values() {
        let response = Response::builder()
            .header("x-frame-options", "SAMEORIGIN")
            .header("content-security-policy", "default-src 'self'")
            .body(Body::empty())
            .expect("response builds");
        let response = with_console_security_headers(response);
        assert_eq!(
            response
                .headers()
                .get("x-frame-options")
                .map(|v| v.as_bytes()),
            Some(b"SAMEORIGIN".as_slice())
        );
        assert_eq!(
            response
                .headers()
                .get("content-security-policy")
                .map(|v| v.as_bytes()),
            Some(b"default-src 'self'".as_slice())
        );
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .map(|v| v.as_bytes()),
            Some(b"nosniff".as_slice())
        );
        assert_eq!(
            response.headers().get_all("x-frame-options").iter().count(),
            1
        );
    }

    #[test]
    fn fallback_answers_ai_gateway_prefix_with_a_json_404() {
        let request = axum::http::Request::builder()
            .uri("/ai/v1/models")
            .body(Body::empty())
            .expect("request builds");
        let response = serve_static_from(&WEBSITE, request);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .map(|v| v.as_bytes()),
            Some(b"application/json".as_slice())
        );
    }
}

#[cfg(test)]
mod embedded_dist_tests {
    /// `WEBSITE` embeds `dist/`, which is untracked. CI restores a target
    /// directory in which the build script already ran, onto a checkout that
    /// has no `dist/`; unless the script asks to rerun when the placeholder is
    /// missing, it stays fresh and `include_dir!` fails as soon as anything
    /// this crate depends on changes. A build script can't host tests of its
    /// own, so this checks what its placeholder path declares to Cargo.
    #[test]
    fn build_script_reruns_when_the_placeholder_dist_is_missing() {
        let build_script = include_str!("../../../build.rs");
        let placeholder = build_script
            .split("fn ensure_placeholder_dist")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("build.rs defines ensure_placeholder_dist");
        let rerun = placeholder
            .find("cargo:rerun-if-changed=")
            .expect("the placeholder path must declare a rerun-if-changed path");
        assert!(
            placeholder[rerun..].contains(r#"join("index.html")"#),
            "the rerun path must be the placeholder index.html"
        );
    }
}
