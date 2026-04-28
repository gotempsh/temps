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
use temps_core::plugin::PluginManager;
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
    OutageDetectionService,
};
use temps_notifications::NotificationsPlugin;
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
use temps_workspace::plugin::WorkspacePlugin;
use tokio::net::TcpListener;
use tracing::{debug, info};

// Multi-node support
use temps_deployments::handlers::nodes::NodeAppState;
use temps_deployments::jobs::node_health_check::{check_node_health, failover_offline_nodes};
use temps_deployments::services::node_service::NodeService;
use utoipa_swagger_ui::SwaggerUi;

// Embed the dist directory at compile time
static WEBSITE: Dir = include_dir!("$CARGO_MANIFEST_DIR/dist");

/// Ensure the system user (id=0) exists in the database.
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

    Ok(api_doc)
}

fn create_swagger_router(plugin_manager: &PluginManager) -> anyhow::Result<Router> {
    let api_doc = create_openapi(plugin_manager)?;
    Ok(Router::new().merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api_doc)))
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

/// Validate GeoLite2-City database exists in multiple locations
/// Checks: current directory → data directory → home directory
/// No system dependencies - database file must be placed manually
fn validate_geolite2_database(default_db_path: &Path) -> anyhow::Result<()> {
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

    // Database not found in any location
    Err(anyhow::anyhow!(
        "❌ GeoLite2-City.mmdb not found\n\n\
        The MaxMind GeoLite2 database is required for geolocation features.\n\n\
        📍 Checked locations (in order):\n\
        1. {}\n\
        2. {}\n\n\
        📥 Setup (once, takes 2 minutes):\n\
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
        search_paths[1].display()
    ))
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
    } = params;
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
    validate_geolite2_database(&geo_db_path)?;
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
    // Must be registered before WorkspacePlugin so WorkspacePlugin can resolve DeploymentTokenService in phase 1.
    debug!("Registering DeploymentsPlugin");
    let deployments_plugin = Box::new(DeploymentsPlugin::new());
    plugin_manager.register_plugin(deployments_plugin);

    // 8.7. WorkspacePlugin - interactive AI workspace sessions.
    // Registered after AgentsPlugin (sandbox provider) and DeploymentsPlugin (deployment token service).
    debug!("Registering WorkspacePlugin");
    let workspace_plugin = Box::new(WorkspacePlugin::new());
    plugin_manager.register_plugin(workspace_plugin);

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

    // AI Gateway Plugin - provides AI provider key management and OpenAI-compatible API
    debug!("Registering AiGatewayPlugin");
    let ai_gateway_plugin = Box::new(temps_ai_gateway::AiGatewayPlugin::new());
    plugin_manager.register_plugin(ai_gateway_plugin);

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

    // Start backup scheduler if BackupService is available
    if let Some(backup_service) = service_context.get_service::<temps_backup::BackupService>() {
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let scheduler_token = cancellation_token.clone();
        let scheduler_service = backup_service.clone();

        tokio::spawn(async move {
            debug!("Starting backup scheduler");
            if let Err(e) = scheduler_service
                .start_backup_scheduler(scheduler_token)
                .await
            {
                tracing::error!("Backup scheduler error: {}", e);
            }
        });

        debug!("Backup scheduler started in background");
        // Note: Currently no graceful shutdown mechanism for cancellation_token
        // In the future, this could be wired to a shutdown signal handler
    }

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
        let data_dir = config.data_dir.clone();
        let monitor = Arc::new(DiskSpaceMonitor::new(
            config_service.clone(),
            notification_service,
            data_dir,
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

    // Start alarm service, outage detection, and container health monitoring
    if let (Some(notification_service), Some(queue_service)) = (
        service_context.get_service::<dyn temps_core::notifications::NotificationService>(),
        service_context.get_service::<dyn temps_core::JobQueue>(),
    ) {
        // Create the alarm service (shared across outage detection and container health)
        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            notification_service.clone(),
            queue_service.clone(),
        ));

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
            let health_monitor = Arc::new(ContainerHealthMonitor::new(
                db.clone(),
                container_deployer,
                alarm_service,
                ContainerHealthConfig::default(),
            ));

            tokio::spawn(async move {
                health_monitor.start().await;
            });

            debug!("Container health monitor started (poll interval: 30s)");
        } else {
            debug!("ContainerDeployer not available - container health monitoring disabled");
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
    let node_app_state = Arc::new(NodeAppState {
        node_service: node_service.clone(),
        db: db.clone(),
        config_service: config_service_for_nodes,
        encryption_service: encryption_service_for_nodes,
    });
    let node_routes =
        temps_deployments::handlers::nodes::configure_routes().with_state(node_app_state);

    // Start periodic node health check with failover (every 60s)
    {
        let health_node_service = node_service.clone();
        let health_db = db.clone();
        let deployment_service_for_failover =
            service_context.get_service::<temps_deployments::DeploymentService>();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let offline_ids = check_node_health(&health_node_service, health_db.as_ref()).await;
                if !offline_ids.is_empty() {
                    tracing::info!(
                        "Node health check: marked {} node(s) as offline",
                        offline_ids.len()
                    );
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

    let app = plugin_manager
        .build_application()
        .map_err(|e| anyhow::anyhow!("Failed to build application: {}", e))?
        .merge(create_swagger_router(&plugin_manager)?)
        .nest("/api", node_routes)
        .nest("/api", route_sync_routes);

    let app = app.fallback(serve_static_file);

    info!("Plugin system initialized successfully with static file serving");

    // Start the HTTP server
    let listener = TcpListener::bind(&config.console_address).await?;
    info!("Console API server listening on {}", config.console_address);

    // Signal that the console API is ready
    if let Some(signal) = ready_signal {
        let _ = signal.send(());
        debug!("Console API ready signal sent");
    }

    // Graceful shutdown: listen for Ctrl+C, then shut down external plugins before exiting.
    // Note: The proxy server has its own CtrlCShutdownSignal. The console API server
    // shuts down external plugins when it receives the same signal.
    let external_plugins_service = plugin_manager
        .service_context()
        .get_service::<temps_external_plugins::ExternalPluginsService>();
    // Use into_make_service_with_connect_info so handlers/middleware can read
    // the immediate peer SocketAddr — required by rate-limiter to decide if
    // X-Forwarded-For headers should be trusted (only from loopback proxies).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Console API received shutdown signal, stopping external plugins...");
        if let Some(service) = external_plugins_service {
            service.shutdown_all().await;
            info!("External plugins shut down");
        }
    })
    .await?;
    info!("Console API server exited");
    Ok(())
}
