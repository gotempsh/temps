use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Router;
use chrono;
use colored::Colorize;
use include_dir::{include_dir, Dir};
use rand::Rng;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use std::future::IntoFuture;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use temps_analytics::AnalyticsPlugin;
use temps_analytics_events::EventsPlugin;
use temps_analytics_funnels::FunnelsPlugin;
use temps_analytics_performance::PerformancePlugin;
use temps_analytics_session_replay::SessionReplayPlugin;
use temps_audit::AuditPlugin;
use temps_auth::{ApiKeyPlugin, AuthPlugin};
use temps_backup::BackupPlugin;
use temps_config::ConfigPlugin;
use temps_config::ServerConfig;
use temps_core::plugin::PluginManager;
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
use temps_logs::LogsPlugin;
use temps_notifications::NotificationsPlugin;
use temps_projects::ProjectsPlugin;
use temps_providers::ProvidersPlugin;
use temps_proxy::ProxyPlugin;
use temps_queue::QueuePlugin;
use temps_screenshots::ScreenshotsPlugin;
use temps_static_files::StaticFilesPlugin;
use temps_status_page::StatusPagePlugin;
use temps_webhooks::WebhooksPlugin;
use tokio::net::TcpListener;
use tracing::{debug, info};
use utoipa_swagger_ui::SwaggerUi;

// Embed the dist directory at compile time
static WEBSITE: Dir = include_dir!("$CARGO_MANIFEST_DIR/dist");

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
    // Get the unified OpenAPI schema from all plugins - fail if it can't be built
    plugin_manager
        .get_unified_openapi()
        .map_err(|e| anyhow::anyhow!("Failed to build unified OpenAPI schema: {}", e))
}

fn create_swagger_router(plugin_manager: &PluginManager) -> anyhow::Result<Router> {
    let api_doc = create_openapi(plugin_manager)?;
    Ok(Router::new().merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api_doc)))
}

/// Static file handler for embedded website
async fn serve_static_file(req: Request) -> Response {
    let path = req.uri().path();

    // Remove leading slash
    let path = path.strip_prefix('/').unwrap_or(path);

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

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime_type)
                .header(header::CACHE_CONTROL, "public, max-age=3600")
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
fn validate_geolite2_database(default_db_path: &PathBuf) -> anyhow::Result<()> {
    // Check multiple locations in order of preference
    let search_paths = vec![
        // 1. Current working directory (most convenient for local development)
        PathBuf::from("./GeoLite2-City.mmdb"),
        // 2. Data directory (from config)
        default_db_path.clone(),
    ];

    // Try to find the database in any of the search paths
    for path in &search_paths {
        if path.exists() {
            debug!("✓ GeoLite2 database found at: {}", path.display());
            return Ok(());
        }
    }

    // Database not found in any location
    return Err(anyhow::anyhow!(
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
    ));
}

/// Initialize and start the console API server
pub async fn start_console_api(
    db: Arc<DbConnection>,
    config: Arc<ServerConfig>,
    cookie_crypto: Arc<CookieCrypto>,
    encryption_service: Arc<EncryptionService>,
    route_table: Arc<temps_proxy::CachedPeerTable>,
    ready_signal: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
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

    // Register plugins in dependency order:
    // 1. ConfigPlugin - provides configuration services
    debug!("Registering ConfigPlugin");
    let config_plugin = Box::new(ConfigPlugin::new(config.clone()));
    plugin_manager.register_plugin(config_plugin);

    // 2. QueuePlugin - provides job queue services
    debug!("Registering QueuePlugin");
    let queue_plugin = Box::new(QueuePlugin::with_default_capacity());
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

    // 7.1. EmailPlugin - provides email sending and domain management (depends on database and encryption)
    debug!("Registering EmailPlugin");
    let email_plugin = Box::new(EmailPlugin::new());
    plugin_manager.register_plugin(email_plugin);

    // 7.5. WebhooksPlugin - provides webhook delivery and management (depends on database and encryption)
    debug!("Registering WebhooksPlugin");
    let webhooks_plugin = Box::new(WebhooksPlugin::new());
    plugin_manager.register_plugin(webhooks_plugin);

    // 4. DomainsPlugin - provides DNS and TLS certificate management (depends on config and database)
    debug!("Registering DomainsPlugin");
    let domains_plugin = Box::new(DomainsPlugin::new());
    plugin_manager.register_plugin(domains_plugin);

    // 4.5. DnsPlugin - provides DNS provider management (depends on database and encryption)
    debug!("Registering DnsPlugin");
    let dns_plugin = Box::new(DnsPlugin::new());
    plugin_manager.register_plugin(dns_plugin);

    // 5. ProvidersPlugin - provides external service management (depends on database and encryption)
    debug!("Registering ProvidersPlugin");
    let providers_plugin = Box::new(ProvidersPlugin::new());
    plugin_manager.register_plugin(providers_plugin);

    // 5.5. EnvironmentsPlugin - provides environment management (depends on config)
    debug!("Registering EnvironmentsPlugin");
    let environments_plugin = Box::new(EnvironmentsPlugin::new());
    plugin_manager.register_plugin(environments_plugin);

    // 6. ProjectsPlugin - provides project management (depends on providers, config, queue)
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

    // 9. DeploymentsPlugin - provides deployment orchestration (depends on deployer and screenshots)
    debug!("Registering DeploymentsPlugin");
    let deployments_plugin = Box::new(DeploymentsPlugin::new());
    plugin_manager.register_plugin(deployments_plugin);

    // 9.5. ImportPlugin - provides workload import functionality (depends on GitPlugin, ProjectsPlugin, DeploymentsPlugin)
    debug!("Registering ImportPlugin");
    let import_plugin = Box::new(ImportPlugin::new());
    plugin_manager.register_plugin(import_plugin);

    // 9.6. StatusPagePlugin - provides status page and monitoring (depends on database and projects)
    debug!("Registering StatusPagePlugin");
    let status_page_plugin = Box::new(StatusPagePlugin::new());
    plugin_manager.register_plugin(status_page_plugin);

    // 10. AuthPlugin - provides authentication and authorization (depends on notification service)
    debug!("Registering AuthPlugin");
    let auth_plugin = Box::new(AuthPlugin::new());
    plugin_manager.register_plugin(auth_plugin);

    // 11. BackupPlugin - provides backup services (depends on database, audit, and notification services, and providers)
    debug!("Registering BackupPlugin");
    let backup_plugin = Box::new(BackupPlugin::new());
    plugin_manager.register_plugin(backup_plugin);

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
        let users = user_service
            .get_all_users(false) // Don't include deleted users
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get users: {}", e))?;

        if users.is_empty() {
            debug!("No users found, creating system user and prompting for admin email");

            // Initialize roles first to ensure they exist
            user_service
                .initialize_roles()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to initialize roles: {}", e))?;
            debug!("Initialized user roles");

            // First, check if system user exists (id = 0)
            let system_user_exists = users::Entity::find_by_id(0)
                .one(db.as_ref())
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
                    .insert(db.as_ref())
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create system user: {}", e))?;
                debug!("Created system user");
            } else {
                debug!("System user already exists, skipping creation");
            }

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

    // Build the application with all plugin routes and OpenAPI schemas
    debug!("Building application with plugin routes");
    let app = plugin_manager
        .build_application()
        .map_err(|e| anyhow::anyhow!("Failed to build application: {}", e))?
        .merge(create_swagger_router(&plugin_manager)?)
        .fallback(serve_static_file);

    info!("Plugin system initialized successfully with static file serving");

    // Start the HTTP server
    let listener = TcpListener::bind(&config.console_address).await?;
    info!("Console API server listening on {}", config.console_address);

    // Signal that the console API is ready
    if let Some(signal) = ready_signal {
        let _ = signal.send(());
        debug!("Console API ready signal sent");
    }

    axum::serve(listener, app).into_future().await?;
    info!("Console API server exited");
    Ok(())
}
