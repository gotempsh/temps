use argon2::password_hash::{rand_core::OsRng, SaltString};
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
use rand::Rng;
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
use temps_deployments::DeploymentsPlugin;
use temps_dns::DnsPlugin;
use temps_domains::DomainsPlugin;
use temps_email::EmailPlugin;
use temps_entities::users;
use temps_environments::EnvironmentsPlugin;
use temps_error_tracking::ErrorTrackingPlugin;
use temps_geo::GeoPlugin;
use temps_git::GitPlugin;
use temps_import::ImportPlugin;
use temps_infra::InfraPlugin;
use temps_kv::KvPlugin;
use temps_log_aggregator::{LogAggregatorPlugin, StorageConfig};
use temps_logs::LogsPlugin;
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
use temps_vulnerability_scanner::VulnerabilityScannerPlugin;
use temps_webhooks::WebhooksPlugin;
use tokio::net::TcpListener;
use tracing::{debug, info};

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
    use sysinfo::SystemExt;
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    // sysinfo 0.29 reports total_memory() in BYTES.
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
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

async fn create_initial_admin_user(
    conn: &sea_orm::DatabaseConnection,
    email: &str,
) -> anyhow::Result<()> {
    use sea_orm::{ActiveModelTrait, ColumnTrait, QueryFilter};

    // Check if user with this email already exists (normalize to lowercase)
    let email_lower = email.to_lowercase();
    let existing_user = users::Entity::find()
        .filter(users::Column::Email.eq(&email_lower))
        .one(conn)
        .await?;

    if existing_user.is_some() {
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

    // Generate a secure random password
    let password = generate_secure_password();

    // Hash the password using Argon2
    let argon2 = Argon2::default();
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();

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

    let user = new_user.insert(conn).await?;

    // Get the admin role
    let admin_role = temps_entities::roles::Entity::find()
        .filter(temps_entities::roles::Column::Name.eq("admin"))
        .one(conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Admin role not found"))?;

    // Assign admin role to the user
    let user_role = temps_entities::user_roles::ActiveModel {
        user_id: Set(user.id),
        role_id: Set(admin_role.id),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    user_role.insert(conn).await?;

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

    // Ask for confirmation before continuing
    loop {
        print!(
            "{} ",
            "Have you saved the password? (y/n):".bright_white().bold()
        );
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
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

    debug!("Created initial admin user with email: {}", email);

    Ok(())
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

    // Basic email validation
    if email.is_empty() || !email.contains('@') || !email.contains('.') {
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

    match WEBSITE.get_file(path) {
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
            if let Some(index) = WEBSITE.get_file("index.html") {
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

/// Source URL for downloading GeoLite2-City.mmdb when missing on startup.
/// Mirrors `setup.rs::GEOLITE2_DOWNLOAD_URL` so `temps serve` recovers a missing
/// database the same way the setup wizard would.
const GEOLITE2_DOWNLOAD_URL: &str =
    "https://raw.githubusercontent.com/gotempsh/temps/refs/heads/main/crates/temps-cli/GeoLite2-City.mmdb";

/// Download GeoLite2-City.mmdb to `dest` from GitHub (same source as `temps setup`).
/// Writes to a sibling `.tmp` file and renames atomically on success.
async fn download_geolite2_database_on_startup(dest: &Path) -> anyhow::Result<()> {
    use futures::StreamExt;
    use std::io::Write;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create data directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    info!(
        "Downloading GeoLite2-City.mmdb from {} to {}",
        GEOLITE2_DOWNLOAD_URL,
        dest.display()
    );

    let response = reqwest::Client::new()
        .get(GEOLITE2_DOWNLOAD_URL)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start GeoLite2 download: {}", e))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to download GeoLite2 database: HTTP {}",
            response.status()
        ));
    }

    let temp_path = dest.with_extension("mmdb.tmp");
    let mut file = std::fs::File::create(&temp_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to create temporary file {}: {}",
            temp_path.display(),
            e
        )
    })?;

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| anyhow::anyhow!("Download error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| anyhow::anyhow!("Failed to write to {}: {}", temp_path.display(), e))?;
        downloaded += chunk.len() as u64;
    }
    drop(file);

    std::fs::rename(&temp_path, dest).map_err(|e| {
        anyhow::anyhow!(
            "Failed to move {} to {}: {}",
            temp_path.display(),
            dest.display(),
            e
        )
    })?;

    if downloaded < 1_000_000 {
        return Err(anyhow::anyhow!(
            "Downloaded GeoLite2 database is too small ({} bytes); file may be corrupted",
            downloaded
        ));
    }

    info!(
        "✓ Downloaded GeoLite2 database to {} ({:.1} MB)",
        dest.display(),
        downloaded as f64 / 1024.0 / 1024.0
    );

    Ok(())
}

/// Validate GeoLite2-City database exists in multiple locations.
/// Checks current directory and data directory; if neither has the file,
/// downloads it to `default_db_path` from the same GitHub URL used by
/// `temps setup`. Errors only after the download attempt fails.
async fn validate_geolite2_database(default_db_path: &Path) -> anyhow::Result<()> {
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
    match download_geolite2_database_on_startup(default_db_path).await {
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
/// - `GET /readyz` — **readiness**: `200 OK` only after plugin two-phase init
///   has completed (the shared `ready` flag is flipped at the same point the
///   legacy oneshot `ready_signal` fires, immediately before `axum::serve`).
///   Returns `503 Service Unavailable` while warming up. This is the gate the
///   split-topology upgrade flow polls before declaring a console upgrade
///   successful — binding the port is NOT sufficient, because the router would
///   otherwise answer 200 while every real route still 500s during warmup.
///
/// The flag lives in an `Arc<AtomicBool>` shared with the serve loop rather
/// than reading the oneshot, so the probe stays truthful for the entire process
/// lifetime (the oneshot fires exactly once and is then consumed).
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
    } = params;

    // Readiness flag for the `/readyz` probe. Starts `false` (not ready) and is
    // flipped to `true` at the same point the legacy `ready_signal` fires —
    // after the full plugin system has initialized and immediately before the
    // listeners begin serving. The health router (mounted on the public surface
    // below) reads this so a supervisor or the split-topology upgrade gate can
    // tell "warming up" (503) from "serving" (200) for the process's lifetime.
    let ready_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // PRE-VALIDATE all plugin dependencies BEFORE initializing plugin manager
    // This ensures clear error messages if any critical resources are missing
    debug!("Pre-validating plugin dependencies...");

    // 1. Validate Docker connectivity
    debug!("Checking Docker daemon connectivity...");
    let docker = match bollard::Docker::connect_with_defaults() {
        Ok(d) => d,
        Err(e) => {
            return Err(anyhow::anyhow!(
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
                Deployment features will not be available until Docker is accessible.",
                e
            ));
        }
    };
    let docker = Arc::new(docker);
    debug!("✓ Docker daemon is accessible");

    // 2. Validate GeoPlugin dependencies (GeoLite2 database)
    debug!("Checking GeoLite2 database...");
    let geo_db_path = config.data_dir.join("GeoLite2-City.mmdb");
    validate_geolite2_database(&geo_db_path).await?;
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
    service_context.register_service(docker.clone());

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
    let template_service = Arc::new(TemplateService::new(Some(templates_override_path)));

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

    // Register plugins in dependency order:
    // 1. ConfigPlugin - provides configuration services
    debug!("Registering ConfigPlugin");
    let config_plugin = Box::new(ConfigPlugin::new(config.clone()));
    plugin_manager.register_plugin(config_plugin);

    // 1.5. TelemetryPlugin - registers the anonymous telemetry reporter
    // (depends only on ServerConfig for the data dir). Registered early so
    // every later plugin can require the Arc<dyn TelemetryReporter>.
    debug!("Registering TelemetryPlugin");
    let telemetry_plugin = Box::new(TelemetryPlugin::new(
        config.clone(),
        env!("CARGO_PKG_VERSION"),
    ));
    plugin_manager.register_plugin(telemetry_plugin);

    // 2. QueuePlugin - registers the pre-created job queue into the service context
    debug!("Registering QueuePlugin");
    let queue_plugin = Box::new(QueuePlugin::new(queue));
    plugin_manager.register_plugin(queue_plugin);

    // 2.5. LogsPlugin - provides logging services (no dependencies)
    debug!("Registering LogsPlugin");
    let logs_dir = config.data_dir.join("logs");
    let logs_plugin = Box::new(LogsPlugin::new(logs_dir));
    plugin_manager.register_plugin(logs_plugin);

    // 3. AnalyticsPlugin - provides analytics services (depends on database)
    debug!("Registering AnalyticsPlugin");
    let analytics_plugin = Box::new(AnalyticsPlugin::new());
    plugin_manager.register_plugin(analytics_plugin);

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

    // 7.5. WebhooksPlugin - provides webhook delivery and management (depends on database and encryption)
    debug!("Registering WebhooksPlugin");
    let webhooks_plugin = Box::new(WebhooksPlugin::new());
    plugin_manager.register_plugin(webhooks_plugin);

    // 5. ProvidersPlugin - provides external service management (depends on database and encryption)
    debug!("Registering ProvidersPlugin");
    let providers_plugin = Box::new(ProvidersPlugin::new());
    plugin_manager.register_plugin(providers_plugin);

    // 5.1. KvPlugin - provides key-value storage (depends on database, docker)
    debug!("Registering KvPlugin");
    let kv_plugin = Box::new(KvPlugin::new());
    plugin_manager.register_plugin(kv_plugin);

    // 5.2. BlobPlugin - provides blob storage (depends on database, docker)
    debug!("Registering BlobPlugin");
    let blob_plugin = Box::new(BlobPlugin::new());
    plugin_manager.register_plugin(blob_plugin);

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
    // MUST be registered before DeploymentsPlugin since deployments depend on vulnerability scanner services
    debug!("Registering VulnerabilityScannerPlugin");
    let vulnerability_scanner_plugin = Box::new(VulnerabilityScannerPlugin::new());
    plugin_manager.register_plugin(vulnerability_scanner_plugin);

    // 8.6. AgentsPlugin - MUST be registered before DeploymentsPlugin so DeploymentsPlugin can
    // resolve AgentSyncService via the plugin context. If registered after, DeploymentsPlugin
    // falls back to NoOpAgentSyncService and agent sync is silently skipped on every deployment.
    debug!("Registering AgentsPlugin");
    let agents_plugin = Box::new(AgentsPlugin::new());
    plugin_manager.register_plugin(agents_plugin);

    // 9. DeploymentsPlugin - provides deployment orchestration (depends on deployer, screenshots, and vulnerability scanner)
    debug!("Registering DeploymentsPlugin");
    let deployments_plugin = Box::new(DeploymentsPlugin::new());
    plugin_manager.register_plugin(deployments_plugin);

    // 8.8. SandboxPlugin - Vercel-compatible `/v1/sandbox/*` API.
    // Consumes the shared SandboxProvider registered by AgentsPlugin.
    debug!("Registering SandboxPlugin");
    let sandbox_plugin = Box::new(SandboxPlugin::new());
    plugin_manager.register_plugin(sandbox_plugin);

    // 9.1. LogAggregatorPlugin - structured log collection, storage, search, and streaming
    // Depends on database, Docker (from DeployerPlugin), and AuditLogger (from AuditPlugin)
    debug!("Registering LogAggregatorPlugin");
    let log_aggregator_storage_config = match std::env::var("TEMPS_LOG_STORAGE_BACKEND")
        .unwrap_or_else(|_| "filesystem".to_string())
        .as_str()
    {
        "s3" => StorageConfig::S3 {
            bucket: std::env::var("TEMPS_LOG_S3_BUCKET")
                .expect("TEMPS_LOG_S3_BUCKET must be set when using S3 storage backend"),
            region: std::env::var("TEMPS_LOG_S3_REGION")
                .unwrap_or_else(|_| "us-east-1".to_string()),
            endpoint: std::env::var("TEMPS_LOG_S3_ENDPOINT").ok(),
            access_key_id: std::env::var("TEMPS_LOG_S3_ACCESS_KEY_ID")
                .expect("TEMPS_LOG_S3_ACCESS_KEY_ID must be set when using S3 storage backend"),
            secret_access_key: std::env::var("TEMPS_LOG_S3_SECRET_ACCESS_KEY")
                .expect("TEMPS_LOG_S3_SECRET_ACCESS_KEY must be set when using S3 storage backend"),
            prefix: Some(
                std::env::var("TEMPS_LOG_S3_PREFIX").unwrap_or_else(|_| "logs/".to_string()),
            ),
            force_path_style: std::env::var("TEMPS_LOG_S3_FORCE_PATH_STYLE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        },
        _ => StorageConfig::Filesystem {
            base_path: config.data_dir.join("log-aggregator"),
        },
    };
    let log_aggregator_plugin = Box::new(LogAggregatorPlugin::new(log_aggregator_storage_config));
    plugin_manager.register_plugin(log_aggregator_plugin);

    // 9.5. ImportPlugin - provides workload import functionality (depends on GitPlugin, ProjectsPlugin, DeploymentsPlugin)
    debug!("Registering ImportPlugin");
    let import_plugin = Box::new(ImportPlugin::new());
    plugin_manager.register_plugin(import_plugin);

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

    // AI Gateway Plugin - provides AI provider key management and OpenAI-compatible API
    debug!("Registering AiGatewayPlugin");
    let ai_gateway_plugin = Box::new(temps_ai_gateway::AiGatewayPlugin::new());
    plugin_manager.register_plugin(ai_gateway_plugin);

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
    );
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

            if let Some(admin_email) = prompt_for_admin_email()? {
                create_initial_admin_user(db.as_ref(), &admin_email).await?;
            } else {
                return Err(anyhow::anyhow!("Valid admin email is required to continue"));
            }
        }
    } else {
        debug!("UserService not available, skipping user initialization");
    }

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

    // Start disk space monitoring if ConfigService and NotificationService are available
    if let (Some(config_service), Some(notification_service)) = (
        service_context.get_service::<temps_config::ConfigService>(),
        service_context.get_service::<dyn temps_core::notifications::NotificationService>(),
    ) {
        let monitor = Arc::new(DiskSpaceMonitor::new(
            config_service.clone(),
            notification_service,
        ));

        tokio::spawn(async move {
            monitor.start_monitoring().await;
        });

        debug!("Disk space monitoring started in background");
    } else {
        tracing::warn!(
            "ConfigService or NotificationService not available - disk space monitoring disabled."
        );
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
    if let (Some(notification_service), Some(external_service_manager)) = (
        service_context.get_service::<dyn temps_core::notifications::NotificationService>(),
        service_context.get_service::<temps_providers::ExternalServiceManager>(),
    ) {
        use temps_providers::health_monitor::{
            ExternalServiceHealthConfig, ExternalServiceHealthMonitor,
        };
        let health_monitor = Arc::new(ExternalServiceHealthMonitor::new(
            db.clone(),
            external_service_manager,
            notification_service,
            ExternalServiceHealthConfig::default(),
            docker.clone(),
            service_context.require_service::<temps_core::EncryptionService>(),
        ));

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
            "NotificationService or ExternalServiceManager not available - external service health monitoring disabled."
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
        notification_service: service_context
            .get_service::<dyn temps_core::notifications::NotificationService>(),
    });
    let node_routes =
        temps_deployments::handlers::nodes::configure_routes().with_state(node_app_state);

    // Start periodic node health check with failover (every 60s)
    {
        let health_node_service = node_service.clone();
        let health_db = db.clone();
        let deployment_service_for_failover =
            service_context.get_service::<temps_deployments::DeploymentService>();
        let health_notification_service =
            service_context.get_service::<dyn temps_core::notifications::NotificationService>();
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
                    if let Some(ref notification_service) = health_notification_service {
                        notify_nodes_offline(
                            &offline_ids,
                            &health_node_service,
                            notification_service,
                        )
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
                if let (Some(ref notification_service), Some(ref config_service)) =
                    (&health_notification_service, &health_config_service)
                {
                    check_node_resources(health_db.as_ref(), config_service, notification_service)
                        .await;
                    // The control plane isn't a `nodes` row, so it's excluded
                    // from the query above — alert on its own metrics separately.
                    check_control_plane_resources(config_service, notification_service).await;
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
                    // ALLOWLIST for the AI `call_api` tool — opt-in / secure by
                    // default. The model may call ONLY these read-only GET
                    // operations; every other endpoint (including any newly added
                    // one) is invisible to it. This is deliberately an allowlist,
                    // not a denylist: GET endpoints across the platform return
                    // decrypted secrets (service params → DB passwords, env-var
                    // reveals, notification configs → Slack/SMTP/Cloudflare creds,
                    // S3 credentials, …), and a hand-maintained denylist can't be
                    // kept exhaustive. Curated for the SRE/debugging use case:
                    // observability, runtime/deploy status, errors, and MASKED
                    // metadata. Adding an entry is a security decision — verify the
                    // operation's response contains NO decrypted secret/token/key.
                    let allowlist: Vec<String> = [
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
                        // ── Service status / health / types (NOT params/env) ──
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
                        "get_today_stats",
                        "get_recent_activity",
                        "get_events_timeline",
                        "get_events_count",
                        "get_page_paths",
                        "get_performance_metrics",
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                    let caller = temps_ai_api_tools::InternalApiCaller::new_allowlisted(
                        split.admin.clone(),
                        &openapi,
                        allowlist,
                    );
                    handle.set(caller);
                    debug!("ADR-024: InternalApiCaller populated in ApiToolsHandle");

                    // ── Propose-then-confirm WRITE tool ──
                    // Populate the separate WriteApiToolsHandle with a method-aware
                    // caller over a CURATED allowlist of mutating operations. The AI
                    // never executes these — calling `temps_write` only stages a
                    // `proposed` ai_pending_actions row; a human confirm endpoint
                    // replays the mutation through this same router (permission_guard!
                    // + audit). The tool itself is also gated per-project behind
                    // projects.ai_write_actions_enabled (default OFF). This allowlist
                    // is conservative by design: high-value, mostly-reversible
                    // lifecycle + config operations. Adding an entry is a product +
                    // security decision (what may the AI propose for a human to run).
                    if let Some(write_handle) =
                        service_context.get_service::<temps_ai_api_tools::WriteApiToolsHandle>()
                    {
                        let write_allowlist: Vec<String> = [
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
                            // ── Environment variables (set / change) ──
                            "create_environment_variable",
                            "update_environment_variable",
                            "delete_environment_variable",
                            // ── Domains (attach / detach at the environment level only;
                            //    account-global domain create/delete excluded) ──
                            "add_environment_domain",
                            "delete_environment_domain",
                        ]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
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
    let public_app = Router::new()
        .merge(health_router(ready_flag.clone()))
        .nest("/api", public_router);
    let admin_app = Router::new()
        .nest("/api", admin_router)
        .fallback(serve_static_file);

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

    info!("Plugin system initialized successfully with static file serving");

    let external_plugins_service = plugin_manager
        .service_context()
        .get_service::<temps_external_plugins::ExternalPluginsService>();

    let shutdown_signal = {
        let svc = external_plugins_service.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Console API received shutdown signal, stopping external plugins...");
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

            // Plugins are fully initialized at this point; flip readiness so
            // `/readyz` answers 200 and notify the legacy oneshot waiter.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(signal) = ready_signal {
                let _ = signal.send(());
                debug!("Console API ready signal sent");
            }

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

            // Plugins are fully initialized at this point; flip readiness so
            // `/readyz` answers 200 and notify the legacy oneshot waiter.
            ready_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(signal) = ready_signal {
                let _ = signal.send(());
                debug!("Console API ready signal sent");
            }

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
