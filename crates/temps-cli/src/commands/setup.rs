//! Setup command for initial Temps configuration
//!
//! This command provisions the necessary components to run Temps:
//! - Admin user with email/password
//! - DNS provider (Cloudflare, Route53, etc.)
//! - Git provider (GitHub) with token
//! - Domain with SSL certificate provisioning

use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use clap::Args;
use colored::Colorize;
use rand::Rng;
use rustls::crypto::CryptoProvider;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use temps_auth::UserService;
use temps_core::{AppSettings, EncryptionService};
use temps_dns::providers::credentials::{
    AzureCredentials, CloudflareCredentials, DigitalOceanCredentials, GcpCredentials,
    ProviderCredentials, Route53Credentials,
};
use temps_domains::dns_provider::{
    CloudflareDnsProvider, DnsPropagationChecker, DnsProviderService,
};
use temps_domains::tls::providers::LetsEncryptProvider;
use temps_domains::tls::repository::DefaultCertificateRepository;
use temps_domains::DomainService;
use temps_entities::{
    dns_providers, domains, git_provider_connections, git_providers, roles, settings, user_roles,
    users,
};
use tracing::{debug, warn};
use x509_parser::prelude::*;

/// Supported DNS providers
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DnsProviderType {
    Cloudflare,
    Route53,
    DigitalOcean,
    Azure,
    Gcp,
}

impl std::fmt::Display for DnsProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsProviderType::Cloudflare => write!(f, "cloudflare"),
            DnsProviderType::Route53 => write!(f, "route53"),
            DnsProviderType::DigitalOcean => write!(f, "digitalocean"),
            DnsProviderType::Azure => write!(f, "azure"),
            DnsProviderType::Gcp => write!(f, "gcp"),
        }
    }
}

/// Output format for the setup command
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output with colors and formatting
    #[default]
    Text,
    /// JSON output for automation and scripting
    Json,
}

#[derive(Args)]
pub struct SetupCommand {
    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Admin user email address (required)
    #[arg(long)]
    pub admin_email: String,

    /// Admin user display name (default: "Admin")
    #[arg(long, default_value = "Admin")]
    pub admin_name: String,

    /// Admin user password (auto-generated if not provided)
    /// For automation, provide a secure password to avoid interactive prompts
    #[arg(long, env = "TEMPS_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    /// Wildcard domain pattern for SSL certificate (e.g., "*.app.example.com")
    #[arg(long)]
    pub wildcard_domain: String,

    /// DNS provider type (required for certificate provisioning via Let's Encrypt)
    /// Optional when using --wildcard-domain-cert with --skip-dns-records
    #[arg(long, value_enum)]
    pub dns_provider: Option<DnsProviderType>,

    /// Cloudflare API token (required for Cloudflare DNS provider)
    #[arg(long, env = "CLOUDFLARE_API_TOKEN")]
    pub cloudflare_token: Option<String>,

    /// AWS Access Key ID (for Route53 DNS provider)
    #[arg(long, env = "AWS_ACCESS_KEY_ID")]
    pub aws_access_key_id: Option<String>,

    /// AWS Secret Access Key (for Route53 DNS provider)
    #[arg(long, env = "AWS_SECRET_ACCESS_KEY")]
    pub aws_secret_access_key: Option<String>,

    /// AWS Region (for Route53 DNS provider)
    #[arg(long, env = "AWS_REGION", default_value = "us-east-1")]
    pub aws_region: String,

    /// DigitalOcean API token
    #[arg(long, env = "DIGITALOCEAN_API_TOKEN")]
    pub digitalocean_token: Option<String>,

    /// Azure Tenant ID
    #[arg(long, env = "AZURE_TENANT_ID")]
    pub azure_tenant_id: Option<String>,

    /// Azure Client ID
    #[arg(long, env = "AZURE_CLIENT_ID")]
    pub azure_client_id: Option<String>,

    /// Azure Client Secret
    #[arg(long, env = "AZURE_CLIENT_SECRET")]
    pub azure_client_secret: Option<String>,

    /// Azure Subscription ID
    #[arg(long, env = "AZURE_SUBSCRIPTION_ID")]
    pub azure_subscription_id: Option<String>,

    /// Azure Resource Group
    #[arg(long, env = "AZURE_RESOURCE_GROUP")]
    pub azure_resource_group: Option<String>,

    /// GCP Service Account Email
    #[arg(long, env = "GCP_SERVICE_ACCOUNT_EMAIL")]
    pub gcp_service_account_email: Option<String>,

    /// GCP Private Key (PEM format)
    #[arg(long, env = "GCP_PRIVATE_KEY")]
    pub gcp_private_key: Option<String>,

    /// GCP Project ID
    #[arg(long, env = "GCP_PROJECT_ID")]
    pub gcp_project_id: Option<String>,

    /// GitHub Personal Access Token (optional, for Git provider integration)
    /// Skip with --skip-git to set up Temps without GitHub integration
    #[arg(long, env = "GITHUB_TOKEN")]
    pub github_token: Option<String>,

    /// Skip Git provider setup (allows running Temps without GitHub)
    /// Useful for testing or when only using Docker image/static file deployments
    #[arg(long, default_value = "false")]
    pub skip_git: bool,

    /// Skip interactive prompts and use defaults
    #[arg(long, default_value = "false")]
    pub non_interactive: bool,

    /// Server public IP address for DNS A records (auto-detected if not provided)
    #[arg(long, env = "SERVER_IP")]
    pub server_ip: Option<String>,

    /// Skip SSL certificate provisioning (useful for testing or when using external certificates)
    #[arg(long, default_value = "false")]
    pub skip_ssl: bool,

    /// Skip DNS A record creation (useful when managing DNS externally)
    #[arg(long, default_value = "false")]
    pub skip_dns_records: bool,

    /// Use Let's Encrypt staging environment (for testing, avoids rate limits)
    #[arg(long, default_value = "false", env = "LETSENCRYPT_STAGING")]
    pub letsencrypt_staging: bool,

    /// DNS propagation wait time in seconds (default: 60)
    #[arg(long, default_value = "60")]
    pub dns_propagation_wait: u32,

    /// Maximum retry attempts for SSL certificate provisioning (default: 3)
    #[arg(long, default_value = "3")]
    pub ssl_max_retries: u32,

    /// Wait time between SSL retry attempts in seconds (default: 30)
    #[arg(long, default_value = "30")]
    pub ssl_retry_wait: u32,

    /// Output format: text (human-readable) or json (machine-readable)
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,

    /// Path to external wildcard certificate file (PEM format)
    /// When provided, skips Let's Encrypt provisioning and uses this certificate
    #[arg(long)]
    pub wildcard_domain_cert: Option<PathBuf>,

    /// Path to external wildcard certificate private key file (PEM format)
    /// Required when --wildcard-domain-cert is provided
    #[arg(long)]
    pub wildcard_domain_key: Option<PathBuf>,

    /// External URL base for the application (e.g., "https://app.example.com")
    /// Used when running behind a reverse proxy or load balancer
    #[arg(long, env = "TEMPS_EXTERNAL_URL")]
    pub external_url: Option<String>,
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

fn get_data_dir(data_dir: &Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = data_dir {
        Ok(dir.clone())
    } else {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        Ok(home.join(".temps"))
    }
}

fn setup_encryption_key(data_dir: &PathBuf) -> anyhow::Result<String> {
    let encryption_key_path = data_dir.join("encryption_key");

    if encryption_key_path.exists() {
        let key = fs::read_to_string(&encryption_key_path)
            .map_err(|e| anyhow::anyhow!("Failed to read encryption key: {}", e))?;
        Ok(key.trim().to_string())
    } else {
        // Generate new encryption key
        let key = EncryptionService::generate_raw_key();
        fs::write(&encryption_key_path, &key)
            .map_err(|e| anyhow::anyhow!("Failed to write encryption key: {}", e))?;
        debug!(
            "Created encryption key at {}",
            encryption_key_path.display()
        );
        Ok(key)
    }
}

/// Result type indicating whether the user was created or password was reset
pub enum AdminUserResult {
    Created(users::Model, String),
    PasswordReset(users::Model, String),
}

async fn create_admin_user(
    conn: &sea_orm::DatabaseConnection,
    email: &str,
    name: &str,
    provided_password: Option<&str>,
) -> anyhow::Result<AdminUserResult> {
    let email_lower = email.to_lowercase();

    // Use provided password or generate a secure one
    let password = provided_password
        .map(|p| p.to_string())
        .unwrap_or_else(generate_secure_password);

    // Hash password with Argon2
    let argon2 = Argon2::default();
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();

    // Check if user exists
    let existing_user = users::Entity::find()
        .filter(users::Column::Email.eq(&email_lower))
        .one(conn)
        .await?;

    if let Some(existing) = existing_user {
        // User exists - reset password
        let mut user_update: users::ActiveModel = existing.into();
        user_update.password_hash = Set(Some(password_hash));
        user_update.updated_at = Set(chrono::Utc::now());
        let updated_user = user_update.update(conn).await?;

        // Ensure user has admin role
        let admin_role = roles::Entity::find()
            .filter(roles::Column::Name.eq("admin"))
            .one(conn)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("Admin role not found. Database may not be properly initialized.")
            })?;

        // Check if user already has admin role
        let has_admin_role = user_roles::Entity::find()
            .filter(user_roles::Column::UserId.eq(updated_user.id))
            .filter(user_roles::Column::RoleId.eq(admin_role.id))
            .one(conn)
            .await?;

        if has_admin_role.is_none() {
            // Assign admin role
            let user_role = user_roles::ActiveModel {
                user_id: Set(updated_user.id),
                role_id: Set(admin_role.id),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
                ..Default::default()
            };
            user_role.insert(conn).await?;
        }

        debug!("Reset password for admin user: {}", email_lower);
        return Ok(AdminUserResult::PasswordReset(updated_user, password));
    }

    // Create new user
    let new_user = users::ActiveModel {
        email: Set(email_lower.clone()),
        name: Set(name.to_string()),
        password_hash: Set(Some(password_hash)),
        email_verified: Set(true),
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

    // Get admin role
    let admin_role = roles::Entity::find()
        .filter(roles::Column::Name.eq("admin"))
        .one(conn)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("Admin role not found. Database may not be properly initialized.")
        })?;

    // Assign admin role
    let user_role = user_roles::ActiveModel {
        user_id: Set(user.id),
        role_id: Set(admin_role.id),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    user_role.insert(conn).await?;

    debug!("Created admin user: {}", email_lower);
    Ok(AdminUserResult::Created(user, password))
}

async fn create_dns_provider(
    conn: &sea_orm::DatabaseConnection,
    encryption_service: &EncryptionService,
    provider_type: &DnsProviderType,
    credentials: ProviderCredentials,
) -> anyhow::Result<dns_providers::Model> {
    let provider_type_str = provider_type.to_string();

    // Check if provider already exists
    let existing = dns_providers::Entity::find()
        .filter(dns_providers::Column::ProviderType.eq(&provider_type_str))
        .one(conn)
        .await?;

    if let Some(provider) = existing {
        debug!("DNS provider '{}' already exists", provider_type_str);
        return Ok(provider);
    }

    // Serialize and encrypt credentials
    let credentials_json = serde_json::to_string(&credentials)
        .map_err(|e| anyhow::anyhow!("Failed to serialize credentials: {}", e))?;
    let encrypted_credentials = encryption_service
        .encrypt_string(&credentials_json)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt credentials: {}", e))?;

    // Create provider
    let new_provider = dns_providers::ActiveModel {
        name: Set(format!("{} DNS Provider", provider_type_str)),
        provider_type: Set(provider_type_str.clone()),
        credentials: Set(encrypted_credentials),
        is_active: Set(true),
        description: Set(Some(format!(
            "Auto-created by temps setup on {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ))),
        last_used_at: Set(None),
        last_error: Set(None),
        ..Default::default()
    };

    let provider = new_provider.insert(conn).await?;
    debug!("Created DNS provider: {}", provider_type_str);
    Ok(provider)
}

/// Result of git provider creation
pub struct GitProviderCreationResult {
    #[allow(dead_code)] // Provider is available for future use (e.g., repository sync)
    pub provider: git_providers::Model,
    pub connection: git_provider_connections::Model,
}

async fn create_git_provider(
    conn: &sea_orm::DatabaseConnection,
    encryption_service: &EncryptionService,
    token: &str,
    github_username: &str,
) -> anyhow::Result<GitProviderCreationResult> {
    // Check if GitHub provider already exists
    let existing = git_providers::Entity::find()
        .filter(git_providers::Column::ProviderType.eq("github"))
        .filter(git_providers::Column::AuthMethod.eq("pat"))
        .one(conn)
        .await?;

    let provider = if let Some(provider) = existing {
        debug!("GitHub provider already exists");
        provider
    } else {
        // Encrypt the token in auth_config
        let auth_config = serde_json::json!({
            "token": encryption_service.encrypt_string(token)
                .map_err(|e| anyhow::anyhow!("Failed to encrypt token: {}", e))?
        });

        // Generate webhook secret
        let mut webhook_secret_bytes = [0u8; 32];
        rand::thread_rng().fill(&mut webhook_secret_bytes);
        let webhook_secret = hex::encode(webhook_secret_bytes);

        // Create provider
        let new_provider = git_providers::ActiveModel {
            name: Set("GitHub".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(Some("https://github.com".to_string())),
            api_url: Set(Some("https://api.github.com".to_string())),
            auth_method: Set("pat".to_string()),
            auth_config: Set(auth_config),
            webhook_secret: Set(Some(webhook_secret)),
            is_active: Set(true),
            is_default: Set(true),
            ..Default::default()
        };

        let provider = new_provider.insert(conn).await?;
        debug!("Created GitHub provider with PAT authentication");
        provider
    };

    // Check if connection for this username already exists
    let existing_connection = git_provider_connections::Entity::find()
        .filter(git_provider_connections::Column::ProviderId.eq(provider.id))
        .filter(git_provider_connections::Column::AccountName.eq(github_username))
        .one(conn)
        .await?;

    let connection = if let Some(connection) = existing_connection {
        debug!("GitHub connection for '{}' already exists", github_username);
        connection
    } else {
        // Encrypt the PAT token for the connection
        let encrypted_token = encryption_service
            .encrypt_string(token)
            .map_err(|e| anyhow::anyhow!("Failed to encrypt token for connection: {}", e))?;

        // Create connection
        let new_connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(None), // No user in CLI setup
            account_name: Set(github_username.to_string()),
            account_type: Set("User".to_string()),
            access_token: Set(Some(encrypted_token)),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(None),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(false),
            last_synced_at: Set(None),
            ..Default::default()
        };

        let connection = new_connection.insert(conn).await?;
        debug!("Created GitHub connection for user '{}'", github_username);
        connection
    };

    Ok(GitProviderCreationResult {
        provider,
        connection,
    })
}

async fn verify_cloudflare_token(token: &str) -> anyhow::Result<bool> {
    let provider = CloudflareDnsProvider::new(token.to_string());
    match provider.test_api_access().await {
        Ok(true) => Ok(true),
        Ok(false) => Err(anyhow::anyhow!(
            "Cloudflare API token is invalid or does not have required permissions"
        )),
        Err(e) => Err(anyhow::anyhow!("Failed to verify Cloudflare token: {}", e)),
    }
}

/// GitHub user info returned from the API
#[derive(Debug, Clone)]
pub struct GitHubUserInfo {
    pub username: String,
}

async fn verify_github_token(token: &str) -> anyhow::Result<GitHubUserInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "temps-setup")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to verify GitHub token: {}", e))?;

    if response.status().is_success() {
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub user response: {}", e))?;

        let username = json
            .get("login")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("GitHub response missing 'login' field"))?
            .to_string();

        Ok(GitHubUserInfo { username })
    } else {
        Err(anyhow::anyhow!(
            "GitHub token is invalid or does not have required permissions (status: {})",
            response.status()
        ))
    }
}

/// Auto-detect the server's public IP address using external services
async fn detect_public_ip() -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Try multiple services
    let services = vec![
        ("https://api.ipify.org?format=json", "ip"),
        ("https://ipinfo.io/json", "ip"),
        ("https://api.myip.com", "ip"),
    ];

    for (url, field) in services {
        match client.get(url).send().await {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    if let Some(ip) = json.get(field).and_then(|v| v.as_str()) {
                        // Validate it looks like an IP address
                        if ip.parse::<std::net::IpAddr>().is_ok() {
                            return Ok(ip.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                debug!("Failed to get IP from {}: {}", url, e);
                continue;
            }
        }
    }

    Err(anyhow::anyhow!(
        "Unable to auto-detect public IP. Please provide --server-ip manually."
    ))
}

fn print_header() {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
    );
    println!(
        "{}",
        "           🚀 Temps Setup Wizard".bright_white().bold()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
    );
    println!();
}

fn print_section(title: &str) {
    println!();
    println!("{}", format!("── {} ──", title).bright_yellow().bold());
    println!();
}

fn print_success(message: &str) {
    println!("{} {}", "✅".bright_green(), message.bright_white());
}

fn print_warning(message: &str) {
    println!("{} {}", "⚠️ ".bright_yellow(), message.bright_yellow());
}

#[allow(dead_code)]
fn print_error(message: &str) {
    println!("{} {}", "❌".bright_red(), message.bright_red());
}

fn print_info(label: &str, value: &str) {
    println!(
        "   {} {}",
        format!("{}:", label).bright_white().bold(),
        value.bright_cyan()
    );
}

fn print_step(step: u32, total: u32, description: &str) {
    println!(
        "   {} {}",
        format!("[{}/{}]", step, total).bright_cyan().bold(),
        description.bright_white()
    );
}

fn print_substep(description: &str) {
    println!("       {} {}", "→".bright_cyan(), description);
}

fn print_spinner_step(description: &str) {
    print!("       {} {}... ", "⏳".bright_yellow(), description);
    std::io::stdout().flush().ok();
}

fn print_spinner_done() {
    println!("{}", "done".bright_green());
}

fn ask_confirmation(prompt: &str) -> anyhow::Result<bool> {
    print!("{} ", prompt.bright_white().bold());
    io::stdout().flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    let response = response.trim().to_lowercase();

    Ok(response == "y" || response == "yes")
}

/// Check if GeoLite2-City.mmdb exists and warn if missing
/// This database is required to run the application but not for setup
fn check_geolite2_database(data_dir: &PathBuf) {
    let geo_db_path = data_dir.join("GeoLite2-City.mmdb");
    let current_dir_path = PathBuf::from("./GeoLite2-City.mmdb");

    // Check both locations
    if geo_db_path.exists() || current_dir_path.exists() {
        print_success("GeoLite2 database found");
        return;
    }

    // Database not found - show warning with download instructions
    print_warning("GeoLite2 database not found");
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
    );
    println!(
        "   {} {}",
        "📍".bright_yellow(),
        "GeoLite2-City.mmdb is required to run the application".bright_white()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
    );
    println!();
    println!(
        "   {} {}",
        "📥".bright_cyan(),
        "Download instructions:".bright_white().bold()
    );
    println!(
        "   {}",
        "1. Visit: https://www.maxmind.com/en/geolite2/geolite2-free-data-sources".bright_white()
    );
    println!(
        "   {}",
        "2. Create a free MaxMind account (if needed)".bright_white()
    );
    println!(
        "   {}",
        "3. Download 'GeoLite2-City' (GZIP format: .tar.gz)".bright_white()
    );
    println!("   {}", "4. Extract and copy the database:".bright_white());
    println!();
    println!("      {} tar xzf GeoLite2-City_*.tar.gz", "$".bright_cyan());
    println!(
        "      {} cp GeoLite2-City_*/GeoLite2-City.mmdb {}",
        "$".bright_cyan(),
        data_dir.display()
    );
    println!();
    println!(
        "   {} Setup can continue, but {} will fail without this file.",
        "ℹ️ ".bright_blue(),
        "temps serve".bright_cyan()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
    );
    println!();
}

/// Validate certificate PEM format and extract expiration time
fn validate_and_parse_certificate(
    cert_pem: &str,
    expected_domain: &str,
) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    use chrono::Utc;

    // Parse PEM certificate
    let (_, pem) = parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to parse certificate PEM: {:?}", e))?;

    // Parse X509 certificate
    let (_, cert) = X509Certificate::from_der(&pem.contents)
        .map_err(|e| anyhow::anyhow!("Failed to parse X509 certificate: {:?}", e))?;

    // Get expiration time
    let not_after = cert.validity().not_after;
    let expiration_timestamp = not_after.timestamp();
    let expiration_time = chrono::DateTime::from_timestamp(expiration_timestamp, 0)
        .ok_or_else(|| anyhow::anyhow!("Invalid certificate expiration timestamp"))?;

    // Check if certificate is expired
    if expiration_time < Utc::now() {
        return Err(anyhow::anyhow!(
            "Certificate is already expired (expired on {})",
            expiration_time.format("%Y-%m-%d %H:%M:%S UTC")
        ));
    }

    // Check certificate domains (CN and SANs)
    let mut cert_domains: Vec<String> = Vec::new();

    // Get Common Name
    if let Some(cn) = cert.subject().iter_common_name().next() {
        if let Ok(cn_str) = cn.as_str() {
            cert_domains.push(cn_str.to_string());
        }
    }

    // Get Subject Alternative Names
    if let Ok(Some(san_ext)) = cert.subject_alternative_name() {
        for name in &san_ext.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                cert_domains.push(dns.to_string());
            }
        }
    }

    // Check if expected domain matches certificate
    let domain_matches = cert_domains.iter().any(|cert_domain| {
        if cert_domain == expected_domain {
            return true;
        }
        // Check wildcard matching
        if cert_domain.starts_with("*.") {
            let cert_suffix = &cert_domain[2..];
            if expected_domain.starts_with("*.") {
                let expected_suffix = &expected_domain[2..];
                return cert_suffix == expected_suffix;
            }
            // Check if expected is a subdomain of wildcard
            if let Some(expected_suffix) = expected_domain
                .strip_prefix(|c: char| c != '.')
                .and_then(|s| s.strip_prefix('.'))
            {
                return cert_suffix == expected_suffix;
            }
        }
        false
    });

    if !domain_matches {
        println!(
            "   {} Certificate domains: {:?}",
            "⚠".bright_yellow(),
            cert_domains
        );
        println!(
            "   {} Expected domain '{}' does not match certificate. Proceeding anyway...",
            "⚠".bright_yellow(),
            expected_domain
        );
    } else {
        print_substep(&format!(
            "{} Certificate domain validated",
            "✓".bright_green()
        ));
    }

    print_substep(&format!(
        "{} Certificate expires: {}",
        "✓".bright_green(),
        expiration_time.format("%Y-%m-%d %H:%M:%S UTC")
    ));

    Ok(expiration_time)
}

/// Validate private key PEM format
fn validate_private_key(key_pem: &str) -> anyhow::Result<()> {
    // Basic PEM format validation
    if !key_pem.contains("-----BEGIN") || !key_pem.contains("-----END") {
        return Err(anyhow::anyhow!(
            "Invalid private key format. Expected PEM format with BEGIN/END markers."
        ));
    }

    // Check for common private key types
    let valid_types = [
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    ];

    let has_valid_type = valid_types.iter().any(|t| key_pem.contains(t));
    if !has_valid_type {
        return Err(anyhow::anyhow!(
            "Unsupported private key type. Expected RSA, EC, or PKCS#8 private key in PEM format."
        ));
    }

    print_substep(&format!(
        "{} Private key format validated",
        "✓".bright_green()
    ));
    Ok(())
}

/// Import external certificate into the database
async fn import_external_certificate(
    db: &sea_orm::DatabaseConnection,
    encryption_service: &EncryptionService,
    domain: &str,
    certificate_pem: &str,
    private_key_pem: &str,
    expiration_time: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<domains::Model> {
    use chrono::Utc;

    let is_wildcard = domain.starts_with("*.");

    // Encrypt the private key
    let encrypted_private_key = encryption_service
        .encrypt_string(private_key_pem)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt private key: {}", e))?;

    // Check if domain already exists
    let existing = domains::Entity::find()
        .filter(domains::Column::Domain.eq(domain))
        .one(db)
        .await?;

    if let Some(existing_domain) = existing {
        // Update existing domain
        let mut domain_update: domains::ActiveModel = existing_domain.into();
        domain_update.certificate = Set(Some(certificate_pem.to_string()));
        domain_update.private_key = Set(Some(encrypted_private_key));
        domain_update.expiration_time = Set(Some(expiration_time));
        domain_update.status = Set("active".to_string());
        domain_update.last_renewed = Set(Some(Utc::now()));
        domain_update.last_error = Set(None);
        domain_update.last_error_type = Set(None);
        domain_update.verification_method = Set("manual".to_string());
        domain_update.updated_at = Set(Utc::now());

        let updated = domain_update.update(db).await?;
        Ok(updated)
    } else {
        // Create new domain
        let new_domain = domains::ActiveModel {
            domain: Set(domain.to_string()),
            certificate: Set(Some(certificate_pem.to_string())),
            private_key: Set(Some(encrypted_private_key)),
            expiration_time: Set(Some(expiration_time)),
            status: Set("active".to_string()),
            is_wildcard: Set(is_wildcard),
            verification_method: Set("manual".to_string()),
            last_renewed: Set(Some(Utc::now())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let created = new_domain.insert(db).await?;
        Ok(created)
    }
}

/// Update application settings with preview domain and external URL
async fn update_app_settings(
    db: &sea_orm::DatabaseConnection,
    preview_domain: &str,
    external_url: Option<&str>,
) -> anyhow::Result<()> {
    use chrono::Utc;

    // Get existing settings or create default
    let existing = settings::Entity::find_by_id(1).one(db).await?;

    let mut app_settings = existing
        .as_ref()
        .map(|r| AppSettings::from_json(r.data.clone()))
        .unwrap_or_default();

    // Update preview_domain
    app_settings.preview_domain = preview_domain.to_string();

    // Update external_url if provided
    if let Some(url) = external_url {
        app_settings.external_url = Some(url.to_string());
    }

    let now = Utc::now();
    let settings_json = app_settings.to_json();

    if let Some(existing_model) = existing {
        // Update existing settings
        let mut active_model: settings::ActiveModel = existing_model.into();
        active_model.data = Set(settings_json);
        active_model.updated_at = Set(now);
        active_model.update(db).await?;
    } else {
        // Create new settings
        let new_settings = settings::ActiveModel {
            id: Set(1),
            data: Set(settings_json),
            created_at: Set(now),
            updated_at: Set(now),
        };
        new_settings.insert(db).await?;
    }

    Ok(())
}

/// Extract preview domain (base domain) from wildcard domain
/// e.g., "*.davidviejo.kfs.es" -> "davidviejo.kfs.es"
fn extract_preview_domain(wildcard_domain: &str) -> String {
    wildcard_domain.trim_start_matches("*.").to_string()
}

impl SetupCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        print_header();

        debug!("Starting Temps setup");

        // Get data directory
        let data_dir = get_data_dir(&self.data_dir)?;

        // Create data directory if it doesn't exist
        if !data_dir.exists() {
            fs::create_dir_all(&data_dir)?;
            debug!("Created data directory: {}", data_dir.display());
        }

        print_info("Data directory", &data_dir.display().to_string());

        // Setup encryption key
        let encryption_key = setup_encryption_key(&data_dir)?;
        let encryption_service = EncryptionService::new(&encryption_key)
            .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?;

        print_success("Encryption key configured");

        // Check for GeoLite2 database (warning only - not required for setup)
        check_geolite2_database(&data_dir);

        // Create tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new()?;

        // Establish database connection (this also runs migrations)
        print_section("Database Setup");
        print_substep("Checking database connectivity...");

        let db = match rt.block_on(temps_database::establish_connection(&self.database_url)) {
            Ok(db) => {
                print_substep(&format!("{} Database reachable", "✓".bright_green()));
                print_substep("Running migrations...");
                print_substep(&format!("{} Migrations applied", "✓".bright_green()));
                db
            }
            Err(e) => {
                let error_msg = e.to_string();
                // Provide more helpful error messages based on the error type
                if error_msg.contains("Cannot connect to database")
                    || error_msg.contains("timed out")
                {
                    println!();
                    println!(
                        "{}",
                        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_red()
                    );
                    println!(
                        "   {} {}",
                        "❌".bright_red(),
                        "Database connection failed".bright_red().bold()
                    );
                    println!(
                        "{}",
                        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_red()
                    );
                    println!();
                    println!("   {}", error_msg.bright_white());
                    println!();
                    println!("   {} Please check:", "💡".bright_yellow());
                    println!("      {} The database server is running", "•".bright_cyan());
                    println!(
                        "      {} The host and port in DATABASE_URL are correct",
                        "•".bright_cyan()
                    );
                    println!(
                        "      {} Firewall rules allow the connection",
                        "•".bright_cyan()
                    );
                    println!(
                        "      {} The database URL format: postgres://user:pass@host:port/db",
                        "•".bright_cyan()
                    );
                    println!();
                    return Err(anyhow::anyhow!("Database connection failed: {}", error_msg));
                } else {
                    return Err(anyhow::anyhow!("Database error: {}", error_msg));
                }
            }
        };
        print_success("Database connected and migrations applied");

        // Initialize roles (admin, user)
        print_section("Initializing Roles");
        let user_service = UserService::new(db.clone());
        rt.block_on(user_service.initialize_roles())
            .map_err(|e| anyhow::anyhow!("Failed to initialize roles: {}", e))?;
        print_success("Default roles initialized (admin, user)");

        // Create admin user (or reset password if user exists)
        print_section("Admin User Setup");
        let (user, password) = match rt.block_on(create_admin_user(
            db.as_ref(),
            &self.admin_email,
            &self.admin_name,
            self.admin_password.as_deref(),
        ))? {
            AdminUserResult::Created(user, password) => {
                print_success("Admin user created");
                (user, password)
            }
            AdminUserResult::PasswordReset(user, password) => {
                print_success("Admin user password reset");
                (user, password)
            }
        };

        // DNS Provider setup (optional when using external certs with --skip-dns-records)
        // Check if DNS provider is needed
        let needs_dns_provider =
            !self.skip_dns_records || (self.wildcard_domain_cert.is_none() && !self.skip_ssl);

        let dns_credentials: Option<ProviderCredentials> = if needs_dns_provider {
            // DNS provider is required
            let dns_provider = self.dns_provider.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "--dns-provider is required for DNS record management or Let's Encrypt certificate provisioning.\n\
                    Use --skip-dns-records with --wildcard-domain-cert/--wildcard-domain-key to skip DNS provider setup."
                )
            })?;

            print_section("DNS Provider Setup");

            let credentials = match dns_provider {
                DnsProviderType::Cloudflare => {
                    let token = self.cloudflare_token.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "--cloudflare-token is required for Cloudflare DNS provider"
                        )
                    })?;
                    println!("   Verifying Cloudflare API token...");
                    rt.block_on(verify_cloudflare_token(token))?;
                    print_success("Cloudflare token verified");

                    ProviderCredentials::Cloudflare(CloudflareCredentials {
                        api_token: token.clone(),
                        account_id: None,
                    })
                }
                DnsProviderType::Route53 => {
                    let access_key = self.aws_access_key_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--aws-access-key-id is required for Route53 DNS provider")
                    })?;
                    let secret_key = self.aws_secret_access_key.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "--aws-secret-access-key is required for Route53 DNS provider"
                        )
                    })?;

                    ProviderCredentials::Route53(Route53Credentials {
                        access_key_id: access_key.clone(),
                        secret_access_key: secret_key.clone(),
                        session_token: None,
                        region: Some(self.aws_region.clone()),
                    })
                }
                DnsProviderType::DigitalOcean => {
                    let token = self.digitalocean_token.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "--digitalocean-token is required for DigitalOcean DNS provider"
                        )
                    })?;

                    ProviderCredentials::DigitalOcean(DigitalOceanCredentials {
                        api_token: token.clone(),
                    })
                }
                DnsProviderType::Azure => {
                    let tenant_id = self.azure_tenant_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--azure-tenant-id is required for Azure DNS provider")
                    })?;
                    let client_id = self.azure_client_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--azure-client-id is required for Azure DNS provider")
                    })?;
                    let client_secret = self.azure_client_secret.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--azure-client-secret is required for Azure DNS provider")
                    })?;
                    let subscription_id = self.azure_subscription_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "--azure-subscription-id is required for Azure DNS provider"
                        )
                    })?;
                    let resource_group = self.azure_resource_group.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--azure-resource-group is required for Azure DNS provider")
                    })?;

                    ProviderCredentials::Azure(AzureCredentials {
                        tenant_id: tenant_id.clone(),
                        client_id: client_id.clone(),
                        client_secret: client_secret.clone(),
                        subscription_id: subscription_id.clone(),
                        resource_group: resource_group.clone(),
                    })
                }
                DnsProviderType::Gcp => {
                    let email = self.gcp_service_account_email.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "--gcp-service-account-email is required for GCP DNS provider"
                        )
                    })?;
                    let private_key = self.gcp_private_key.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--gcp-private-key is required for GCP DNS provider")
                    })?;
                    let project_id = self.gcp_project_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("--gcp-project-id is required for GCP DNS provider")
                    })?;

                    ProviderCredentials::Gcp(GcpCredentials {
                        service_account_email: email.clone(),
                        private_key: private_key.clone(),
                        project_id: project_id.clone(),
                    })
                }
            };

            let _dns_provider = rt.block_on(create_dns_provider(
                db.as_ref(),
                &encryption_service,
                dns_provider,
                credentials.clone(),
            ))?;
            print_success(&format!("{} DNS provider configured", dns_provider));

            Some(credentials)
        } else {
            print_section("DNS Provider Setup");
            print_info(
                "Skipped",
                "DNS provider not required (using external certificate with --skip-dns-records)",
            );
            None
        };

        // Extract Cloudflare token for DNS A record operations (if available)
        let cloudflare_token_for_dns: Option<String> =
            dns_credentials.as_ref().and_then(|creds| match creds {
                ProviderCredentials::Cloudflare(cf) => Some(cf.api_token.clone()),
                _ => None,
            });

        // GitHub Provider setup (optional)
        if self.skip_git {
            print_section("Git Provider Setup");
            print_info("Skipped", "Git provider setup skipped (--skip-git flag)");
            println!(
                "   {} You can deploy using Docker images or static files without Git integration",
                "💡".bright_yellow()
            );
        } else if let Some(ref github_token) = self.github_token {
            print_section("Git Provider Setup");

            println!("   Verifying GitHub token...");
            let github_user = rt.block_on(verify_github_token(github_token))?;
            print_success(&format!(
                "GitHub token verified (user: {})",
                github_user.username
            ));

            let git_result = rt.block_on(create_git_provider(
                db.as_ref(),
                &encryption_service,
                github_token,
                &github_user.username,
            ))?;
            print_success("GitHub provider configured");
            print_success(&format!(
                "GitHub connection created for '{}'",
                git_result.connection.account_name
            ));
        } else {
            print_section("Git Provider Setup");
            print_info("Skipped", "No GitHub token provided");
            println!(
                "   {} You can add a Git provider later or deploy using Docker images/static files",
                "💡".bright_yellow()
            );
        }

        // Wildcard Domain setup (required)
        print_section("Wildcard Domain Setup");

        // Validate domain format
        if !self.wildcard_domain.starts_with("*.") {
            return Err(anyhow::anyhow!(
                "Domain must be a wildcard domain (e.g., *.app.example.com)"
            ));
        }

        // Validate external certificate arguments are provided together
        if self.wildcard_domain_cert.is_some() != self.wildcard_domain_key.is_some() {
            return Err(anyhow::anyhow!(
                "Both --wildcard-domain-cert and --wildcard-domain-key must be provided together"
            ));
        }

        // Check if external certificate is provided
        if let (Some(cert_path), Some(key_path)) =
            (&self.wildcard_domain_cert, &self.wildcard_domain_key)
        {
            print_section("External Certificate Import");
            println!(
                "   {} Importing external certificate for: {}",
                "🔐".bright_yellow(),
                self.wildcard_domain.bright_cyan().bold()
            );
            println!();

            // Read certificate file
            print_substep(&format!(
                "Reading certificate from: {}",
                cert_path.display()
            ));
            let certificate_pem = fs::read_to_string(cert_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read certificate file '{}': {}",
                    cert_path.display(),
                    e
                )
            })?;

            // Read private key file
            print_substep(&format!("Reading private key from: {}", key_path.display()));
            let private_key_pem = fs::read_to_string(key_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read private key file '{}': {}",
                    key_path.display(),
                    e
                )
            })?;

            // Validate certificate format and extract expiration
            let expiration_time =
                validate_and_parse_certificate(&certificate_pem, &self.wildcard_domain)?;

            // Validate private key format
            validate_private_key(&private_key_pem)?;

            // Import certificate into database
            print_substep("Importing certificate into database...");
            rt.block_on(import_external_certificate(
                db.as_ref(),
                &encryption_service,
                &self.wildcard_domain,
                &certificate_pem,
                &private_key_pem,
                expiration_time,
            ))?;
            print_substep(&format!(
                "{} Certificate imported successfully",
                "✓".bright_green()
            ));
            println!();

            // Handle DNS records if not skipped
            if !self.skip_dns_records {
                let server_ip = if let Some(ip) = &self.server_ip {
                    ip.clone()
                } else if self.non_interactive {
                    return Err(anyhow::anyhow!(
                        "Server IP is required for DNS A records. Use --server-ip or --skip-dns-records"
                    ));
                } else {
                    match rt.block_on(detect_public_ip()) {
                        Ok(ip) => ip,
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Failed to detect IP: {}. Use --server-ip or --skip-dns-records",
                                e
                            ));
                        }
                    }
                };

                // Extract base domain from wildcard
                let base_domain = self
                    .wildcard_domain
                    .trim_start_matches("*.")
                    .split('.')
                    .rev()
                    .take(2)
                    .collect::<Vec<&str>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<&str>>()
                    .join(".");

                let subdomain_prefix = {
                    let without_wildcard = self.wildcard_domain.trim_start_matches("*.");
                    let parts: Vec<&str> = without_wildcard.split('.').collect();
                    if parts.len() > 2 {
                        parts[..parts.len() - 2].join(".")
                    } else {
                        String::new()
                    }
                };

                let cf_provider = CloudflareDnsProvider::new(cloudflare_token_for_dns.clone().ok_or_else(|| {
                    anyhow::anyhow!("DNS A record creation requires Cloudflare provider. Use --skip-dns-records to skip.")
                })?);

                print_section("DNS A Record Setup");
                let wildcard_record_name = if subdomain_prefix.is_empty() {
                    "*".to_string()
                } else {
                    format!("*.{}", subdomain_prefix)
                };
                print_substep(&format!(
                    "Creating A record: {} → {}",
                    format!("{}.{}", wildcard_record_name, base_domain).bright_cyan(),
                    server_ip.bright_yellow()
                ));
                if let Err(e) = rt.block_on(cf_provider.set_a_record(
                    &base_domain,
                    &wildcard_record_name,
                    &server_ip,
                )) {
                    warn!("Failed to create wildcard A record: {}", e);
                } else {
                    print_substep(&format!("{} Wildcard A record created", "✓".bright_green()));
                }

                if !subdomain_prefix.is_empty() {
                    print_substep(&format!(
                        "Creating A record: {} → {}",
                        format!("{}.{}", subdomain_prefix, base_domain).bright_cyan(),
                        server_ip.bright_yellow()
                    ));
                    if let Err(e) = rt.block_on(cf_provider.set_a_record(
                        &base_domain,
                        &subdomain_prefix,
                        &server_ip,
                    )) {
                        warn!("Failed to create subdomain A record: {}", e);
                    } else {
                        print_substep(&format!(
                            "{} Subdomain A record created",
                            "✓".bright_green()
                        ));
                    }
                }
                println!();
            } else {
                print_success("DNS A record creation skipped (--skip-dns-records)");
                println!();
            }

            // Update application settings with preview domain and external URL
            print_section("Application Settings");
            let preview_domain = extract_preview_domain(&self.wildcard_domain);
            rt.block_on(update_app_settings(
                db.as_ref(),
                &preview_domain,
                self.external_url.as_deref(),
            ))?;
            print_success(&format!(
                "Preview domain set to: {}",
                preview_domain.bright_cyan()
            ));
            if let Some(ref url) = self.external_url {
                print_success(&format!("External URL set to: {}", url.bright_cyan()));
            }
            println!();

            return finish_setup(
                &user,
                &password,
                &self.wildcard_domain,
                self.non_interactive,
                &self.output_format,
            );
        }

        // Check if SSL provisioning is skipped
        if self.skip_ssl {
            print_success("SSL certificate provisioning skipped (--skip-ssl)");
            println!();

            // Still need to handle DNS records if not skipped
            if !self.skip_dns_records {
                // We need server IP for DNS records
                let server_ip = if let Some(ip) = &self.server_ip {
                    ip.clone()
                } else if self.non_interactive {
                    return Err(anyhow::anyhow!(
                        "Server IP is required for DNS A records. Use --server-ip or --skip-dns-records"
                    ));
                } else {
                    match rt.block_on(detect_public_ip()) {
                        Ok(ip) => ip,
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Failed to detect IP: {}. Use --server-ip or --skip-dns-records",
                                e
                            ));
                        }
                    }
                };

                // Extract base domain from wildcard
                let base_domain = self
                    .wildcard_domain
                    .trim_start_matches("*.")
                    .split('.')
                    .rev()
                    .take(2)
                    .collect::<Vec<&str>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<&str>>()
                    .join(".");

                let subdomain_prefix = {
                    let without_wildcard = self.wildcard_domain.trim_start_matches("*.");
                    let parts: Vec<&str> = without_wildcard.split('.').collect();
                    if parts.len() > 2 {
                        parts[..parts.len() - 2].join(".")
                    } else {
                        String::new()
                    }
                };

                let cf_provider = CloudflareDnsProvider::new(cloudflare_token_for_dns.clone().ok_or_else(|| {
                    anyhow::anyhow!("DNS A record creation requires Cloudflare provider. Use --skip-dns-records to skip.")
                })?);

                print_section("DNS A Record Setup");
                let wildcard_record_name = if subdomain_prefix.is_empty() {
                    "*".to_string()
                } else {
                    format!("*.{}", subdomain_prefix)
                };
                print_substep(&format!(
                    "Creating A record: {} → {}",
                    format!("{}.{}", wildcard_record_name, base_domain).bright_cyan(),
                    server_ip.bright_yellow()
                ));
                if let Err(e) = rt.block_on(cf_provider.set_a_record(
                    &base_domain,
                    &wildcard_record_name,
                    &server_ip,
                )) {
                    warn!("Failed to create wildcard A record: {}", e);
                } else {
                    print_substep(&format!("{} Wildcard A record created", "✓".bright_green()));
                }

                if !subdomain_prefix.is_empty() {
                    print_substep(&format!(
                        "Creating A record: {} → {}",
                        format!("{}.{}", subdomain_prefix, base_domain).bright_cyan(),
                        server_ip.bright_yellow()
                    ));
                    if let Err(e) = rt.block_on(cf_provider.set_a_record(
                        &base_domain,
                        &subdomain_prefix,
                        &server_ip,
                    )) {
                        warn!("Failed to create subdomain A record: {}", e);
                    } else {
                        print_substep(&format!(
                            "{} Subdomain A record created",
                            "✓".bright_green()
                        ));
                    }
                }
                println!();
            } else {
                print_success("DNS A record creation skipped (--skip-dns-records)");
                println!();
            }

            // Update application settings with preview domain and external URL
            print_section("Application Settings");
            let preview_domain = extract_preview_domain(&self.wildcard_domain);
            rt.block_on(update_app_settings(
                db.as_ref(),
                &preview_domain,
                self.external_url.as_deref(),
            ))?;
            print_success(&format!(
                "Preview domain set to: {}",
                preview_domain.bright_cyan()
            ));
            if let Some(ref url) = self.external_url {
                print_success(&format!("External URL set to: {}", url.bright_cyan()));
            }
            println!();

            return finish_setup(
                &user,
                &password,
                &self.wildcard_domain,
                self.non_interactive,
                &self.output_format,
            );
        }

        // SSL Certificate Provisioning
        print_section("SSL Certificate Provisioning");
        println!(
            "   {} Provisioning wildcard SSL certificate for: {}",
            "🔐".bright_yellow(),
            self.wildcard_domain.bright_cyan().bold()
        );
        println!();

        // Initialize TLS crypto provider (required by rustls)
        let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());

        // Step 1: Verify Cloudflare zone and detect server IP
        print_step(1, 8, "Verifying Cloudflare zone and detecting server IP");
        let cf_provider = CloudflareDnsProvider::new(cloudflare_token_for_dns.clone().ok_or_else(|| {
                    anyhow::anyhow!("DNS A record creation requires Cloudflare provider. Use --skip-dns-records to skip.")
                })?);

        // Extract base domain from wildcard (e.g., "*.app.example.com" -> "example.com")
        let base_domain = self
            .wildcard_domain
            .trim_start_matches("*.")
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<&str>>()
            .into_iter()
            .rev()
            .collect::<Vec<&str>>()
            .join(".");

        // Extract the subdomain prefix (e.g., "*.app.example.com" -> "app", "*.example.com" -> "")
        let subdomain_prefix = {
            let without_wildcard = self.wildcard_domain.trim_start_matches("*.");
            let parts: Vec<&str> = without_wildcard.split('.').collect();
            if parts.len() > 2 {
                // Has subdomain prefix: app.example.com -> "app"
                parts[..parts.len() - 2].join(".")
            } else {
                // No subdomain prefix: example.com -> ""
                String::new()
            }
        };

        print_substep(&format!("Checking zone for '{}'", base_domain));
        let zone_exists = rt.block_on(cf_provider.supports_automatic_challenges(&base_domain));
        if !zone_exists {
            println!();
            return Err(anyhow::anyhow!(
                "❌ Cloudflare zone not found for domain '{}'. \n\
                 Please ensure the domain '{}' is added to your Cloudflare account \n\
                 and the API token has DNS Edit permissions for this zone.",
                base_domain,
                base_domain
            ));
        }
        print_substep(&format!("{} Zone verified", "✓".bright_green()));

        // Detect or use provided server IP
        let server_ip = if let Some(ip) = &self.server_ip {
            print_substep(&format!("Using provided server IP: {}", ip.bright_cyan()));
            ip.clone()
        } else {
            print_spinner_step("Auto-detecting public IP");
            match rt.block_on(detect_public_ip()) {
                Ok(detected_ip) => {
                    print_spinner_done();
                    print_substep(&format!(
                        "Detected public IP: {}",
                        detected_ip.bright_cyan()
                    ));

                    // Always ask for confirmation - IP is critical for DNS setup
                    println!();
                    if !ask_confirmation(&format!(
                        "   Is {} the correct public IP for this server? (y/n):",
                        detected_ip.bright_cyan()
                    ))? {
                        println!();
                        print!("   {} ", "Enter the correct IP address:".bright_white());
                        io::stdout().flush()?;
                        let mut custom_ip = String::new();
                        io::stdin().read_line(&mut custom_ip)?;
                        let custom_ip = custom_ip.trim().to_string();

                        // Validate the IP
                        if custom_ip.parse::<std::net::IpAddr>().is_err() {
                            return Err(anyhow::anyhow!("❌ Invalid IP address: {}", custom_ip));
                        }
                        custom_ip
                    } else {
                        detected_ip
                    }
                }
                Err(e) => {
                    println!("failed");
                    println!();
                    return Err(anyhow::anyhow!(
                        "❌ {}\n\
                         Please provide the server's public IP using --server-ip=<IP>",
                        e
                    ));
                }
            }
        };
        println!();

        // Step 2: Initialize certificate services
        print_step(2, 8, "Initializing certificate services");

        // Set Let's Encrypt environment based on --letsencrypt-staging flag
        if self.letsencrypt_staging {
            std::env::set_var("LETSENCRYPT_MODE", "staging");
            print_substep(&format!(
                "{} Using Let's Encrypt staging environment",
                "ℹ".bright_blue()
            ));
        }

        let encryption_service_arc = std::sync::Arc::new(encryption_service);
        let repository: std::sync::Arc<dyn temps_domains::tls::CertificateRepository> =
            std::sync::Arc::new(DefaultCertificateRepository::new(
                db.clone(),
                encryption_service_arc.clone(),
            ));
        let cert_provider: std::sync::Arc<dyn temps_domains::tls::CertificateProvider> =
            std::sync::Arc::new(LetsEncryptProvider::new(repository.clone()));
        let domain_service = DomainService::new(
            db.clone(),
            cert_provider,
            repository,
            encryption_service_arc,
        );
        print_substep(&format!("{} Services ready", "✓".bright_green()));
        println!();

        // Step 3: Create or get existing domain
        print_step(3, 8, "Creating domain record");
        let domain =
            match rt.block_on(domain_service.create_domain(&self.wildcard_domain, "dns-01")) {
                Ok(d) => {
                    print_substep(&format!(
                        "{} Domain '{}' registered",
                        "✓".bright_green(),
                        self.wildcard_domain
                    ));
                    d
                }
                Err(e) => {
                    // Domain might already exist
                    if e.to_string().contains("already exists") {
                        print_substep(&format!(
                            "{} Domain already exists, using existing record",
                            "ℹ".bright_blue()
                        ));
                        rt.block_on(async {
                            domains::Entity::find()
                                .filter(domains::Column::Domain.eq(&self.wildcard_domain))
                                .one(db.as_ref())
                                .await
                        })?
                        .ok_or_else(|| anyhow::anyhow!("Domain not found after creation error"))?
                    } else {
                        println!();
                        return Err(anyhow::anyhow!("Failed to create domain: {}", e));
                    }
                }
            };
        println!();

        // Check if domain already has a valid certificate
        if domain.status == "active" && domain.certificate.is_some() {
            println!(
                "   {} Domain '{}' already has a valid certificate!",
                "🎉".bright_green(),
                self.wildcard_domain.bright_cyan()
            );
            println!();

            // Update application settings with preview domain and external URL
            print_section("Application Settings");
            let preview_domain = extract_preview_domain(&self.wildcard_domain);
            rt.block_on(update_app_settings(
                db.as_ref(),
                &preview_domain,
                self.external_url.as_deref(),
            ))?;
            print_success(&format!(
                "Preview domain set to: {}",
                preview_domain.bright_cyan()
            ));
            if let Some(ref url) = self.external_url {
                print_success(&format!("External URL set to: {}", url.bright_cyan()));
            }
            println!();

            return finish_setup(
                &user,
                &password,
                &self.wildcard_domain,
                self.non_interactive,
                &self.output_format,
            );
        }

        // Step 4: Request DNS-01 challenge from Let's Encrypt
        print_step(4, 8, "Requesting Let's Encrypt DNS-01 challenge");
        print_spinner_step("Contacting Let's Encrypt ACME server");
        let challenge_data = rt
            .block_on(domain_service.request_challenge(&self.wildcard_domain, &self.admin_email))?;
        print_spinner_done();

        if challenge_data.status == "completed" {
            println!(
                "   {} Certificate provisioned immediately!",
                "🎉".bright_green()
            );
            println!();

            // Update application settings with preview domain and external URL
            print_section("Application Settings");
            let preview_domain = extract_preview_domain(&self.wildcard_domain);
            rt.block_on(update_app_settings(
                db.as_ref(),
                &preview_domain,
                self.external_url.as_deref(),
            ))?;
            print_success(&format!(
                "Preview domain set to: {}",
                preview_domain.bright_cyan()
            ));
            if let Some(ref url) = self.external_url {
                print_success(&format!("External URL set to: {}", url.bright_cyan()));
            }
            println!();

            return finish_setup(
                &user,
                &password,
                &self.wildcard_domain,
                self.non_interactive,
                &self.output_format,
            );
        }

        print_substep(&format!(
            "{} Challenge received - {} TXT record(s) required",
            "✓".bright_green(),
            challenge_data.txt_records.len()
        ));
        println!();

        // Step 5: Auto-provision DNS TXT records via Cloudflare
        print_step(5, 8, "Creating DNS TXT records via Cloudflare");

        // First, clean up any existing TXT records from previous challenge attempts
        // This ensures we don't have stale records that could interfere with validation
        let unique_txt_names: std::collections::HashSet<_> = challenge_data
            .txt_records
            .iter()
            .map(|r| r.name.clone())
            .collect();

        for txt_name in &unique_txt_names {
            print_substep(&format!(
                "Cleaning up old TXT records for: {}",
                txt_name.bright_cyan()
            ));
            if let Err(e) = rt.block_on(cf_provider.remove_txt_record(&base_domain, txt_name)) {
                warn!("Failed to clean up old TXT records for {}: {}", txt_name, e);
                // Continue anyway - old records might not exist
            }
        }

        // Now create all the new TXT records for the current challenge
        for (idx, txt_record) in challenge_data.txt_records.iter().enumerate() {
            print_substep(&format!(
                "Adding TXT record {}/{}: {}",
                idx + 1,
                challenge_data.txt_records.len(),
                txt_record.name.bright_cyan()
            ));
            rt.block_on(cf_provider.set_txt_record(
                &base_domain,
                &txt_record.name,
                &txt_record.value,
            ))
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create DNS TXT record '{}': {}",
                    txt_record.name,
                    e
                )
            })?;
        }
        print_substep(&format!("{} All TXT records created", "✓".bright_green()));
        println!();

        // Step 6: Wait for DNS propagation with active verification using multiple DNS servers
        print_step(6, 8, "Verifying DNS propagation across multiple servers");
        let propagation_checker = DnsPropagationChecker::new();

        // Build the full record name for checking (e.g., _acme-challenge.app.example.com)
        let expected_values: Vec<String> = challenge_data
            .txt_records
            .iter()
            .map(|r| r.value.clone())
            .collect();

        // Get the TXT record name (should be the same for all records in a wildcard challenge)
        let txt_record_name = if let Some(first_txt) = challenge_data.txt_records.first() {
            first_txt.name.clone()
        } else {
            format!("_acme-challenge.{}", self.wildcard_domain)
        };

        print_substep(&format!(
            "Checking TXT record: {}",
            txt_record_name.bright_cyan()
        ));
        print_substep(&format!(
            "Expected {} value(s), requiring 50% server agreement",
            expected_values.len()
        ));

        // Use active propagation verification with polling
        // Wait up to dns_propagation_wait seconds, polling every 10 seconds
        // Require at least 50% of DNS servers to see all records
        let propagation_result = rt.block_on(propagation_checker.wait_for_propagation(
            &txt_record_name,
            &expected_values,
            50, // min_propagation_percent
            self.dns_propagation_wait,
            10, // poll_interval_seconds
        ));

        match propagation_result {
            Some(result) => {
                // Display per-server results
                println!();
                print_substep("DNS Server Propagation Status:");
                for server_result in &result.server_results {
                    let status_icon = if server_result.found {
                        "✓".bright_green()
                    } else {
                        "✗".bright_red()
                    };
                    let error_info = server_result
                        .error
                        .as_ref()
                        .map(|e| format!(" ({})", e))
                        .unwrap_or_default();
                    print_substep(&format!(
                        "   {} {} ({}): {}{}",
                        status_icon,
                        server_result.server_name,
                        server_result.server_ip.bright_black(),
                        if server_result.found {
                            "Found".bright_green()
                        } else {
                            "Not found".bright_yellow()
                        },
                        error_info.bright_black()
                    ));
                }

                if result.is_propagated {
                    print_substep(&format!(
                        "{} DNS propagation verified: {}% of servers see the records",
                        "✓".bright_green(),
                        result.propagation_percentage
                    ));
                } else {
                    print_substep(&format!(
                        "{} Partial propagation: {}% of servers see the records (proceeding anyway)",
                        "⚠".bright_yellow(),
                        result.propagation_percentage
                    ));
                }
            }
            None => {
                print_substep(&format!(
                    "{} DNS propagation verification timed out (proceeding anyway)",
                    "⚠".bright_yellow()
                ));
            }
        }
        println!();

        // Step 7: Complete challenge and get certificate (with retries)
        print_step(7, 8, "Completing DNS challenge with Let's Encrypt");

        let mut certificate_success = false;
        let max_retries = self.ssl_max_retries;
        let retry_wait_seconds = self.ssl_retry_wait;

        // Store the current challenge data - will be updated on retry if we need a new order
        let mut current_challenge_data = challenge_data.clone();

        for attempt in 1..=max_retries {
            if attempt > 1 {
                print_substep(&format!(
                    "Retry {}/{} - creating new ACME order and waiting {} seconds for DNS propagation...",
                    attempt, max_retries, retry_wait_seconds
                ));

                // CRITICAL: On retry, we need to cancel the old order and create a new one
                // ACME orders move to 'invalid' state after validation failure and cannot be reused
                print_substep("Canceling previous order and requesting fresh challenge...");
                if let Err(e) = rt.block_on(domain_service.cancel_order(&self.wildcard_domain)) {
                    warn!("Failed to cancel previous order (may not exist): {}", e);
                }

                // Request a new challenge (this creates a new ACME order)
                match rt.block_on(
                    domain_service.request_challenge(&self.wildcard_domain, &self.admin_email),
                ) {
                    Ok(new_challenge) => {
                        if new_challenge.status == "completed" {
                            // Certificate was issued immediately (unlikely but possible)
                            print_substep(&format!(
                                "{} Certificate provisioned!",
                                "✓".bright_green()
                            ));
                            certificate_success = true;
                            break;
                        }

                        // Update DNS TXT records for the new challenge
                        print_substep("Updating DNS TXT records for new challenge...");

                        // First, clean up old TXT records
                        for txt_record in &current_challenge_data.txt_records {
                            if let Err(e) = rt.block_on(
                                cf_provider.remove_txt_record(&base_domain, &txt_record.name),
                            ) {
                                warn!("Failed to remove old TXT record {}: {}", txt_record.name, e);
                            }
                        }

                        // Add new TXT records
                        for txt_record in &new_challenge.txt_records {
                            if let Err(e) = rt.block_on(cf_provider.set_txt_record(
                                &base_domain,
                                &txt_record.name,
                                &txt_record.value,
                            )) {
                                warn!("Failed to create TXT record {}: {}", txt_record.name, e);
                            }
                        }

                        current_challenge_data = new_challenge;
                        print_substep(&format!("{} New TXT records created", "✓".bright_green()));
                    }
                    Err(e) => {
                        print_substep(&format!(
                            "{} Failed to create new challenge: {}",
                            "⚠".bright_yellow(),
                            e
                        ));
                        // Continue with wait and retry anyway
                    }
                }

                // Wait for DNS propagation with active verification
                print_substep("Verifying DNS propagation for retry...");
                let retry_expected_values: Vec<String> = current_challenge_data
                    .txt_records
                    .iter()
                    .map(|r| r.value.clone())
                    .collect();
                let retry_txt_name = current_challenge_data
                    .txt_records
                    .first()
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| format!("_acme-challenge.{}", self.wildcard_domain));

                let retry_result = rt.block_on(propagation_checker.wait_for_propagation(
                    &retry_txt_name,
                    &retry_expected_values,
                    50, // min_propagation_percent
                    retry_wait_seconds,
                    10, // poll_interval_seconds
                ));

                if let Some(result) = retry_result {
                    // Show brief status for retry
                    for server_result in &result.server_results {
                        let status_icon = if server_result.found {
                            "✓".bright_green()
                        } else {
                            "✗".bright_red()
                        };
                        print_substep(&format!(
                            "   {} {}: {}",
                            status_icon,
                            server_result.server_name,
                            if server_result.found {
                                "Found"
                            } else {
                                "Not found"
                            }
                        ));
                    }
                    print_substep(&format!(
                        "Propagation: {}% of servers",
                        result.propagation_percentage
                    ));
                }
            }

            print_spinner_step(&format!(
                "Requesting certificate issuance (attempt {}/{})",
                attempt, max_retries
            ));
            match rt.block_on(
                domain_service.complete_challenge(&self.wildcard_domain, &self.admin_email),
            ) {
                Ok(completed_domain) => {
                    print_spinner_done();
                    if completed_domain.status == "active" && completed_domain.certificate.is_some()
                    {
                        print_substep(&format!(
                            "{} SSL certificate issued successfully!",
                            "✓".bright_green()
                        ));
                        certificate_success = true;
                        println!();

                        // Clean up DNS TXT records
                        print_substep("Cleaning up DNS TXT records...");
                        for txt_record in &current_challenge_data.txt_records {
                            if let Err(e) = rt.block_on(
                                cf_provider.remove_txt_record(&base_domain, &txt_record.name),
                            ) {
                                warn!("Failed to remove TXT record {}: {}", txt_record.name, e);
                            }
                        }
                        print_substep(&format!("{} DNS cleanup completed", "✓".bright_green()));
                        println!();
                        break;
                    } else {
                        print_substep(&format!(
                            "{} Certificate not ready yet (status: {})",
                            "⚠".bright_yellow(),
                            completed_domain.status
                        ));
                        if attempt == max_retries {
                            println!();
                        }
                    }
                }
                Err(e) => {
                    println!("failed");
                    if attempt < max_retries {
                        print_substep(&format!(
                            "{} Attempt {} failed: {}",
                            "⚠".bright_yellow(),
                            attempt,
                            e
                        ));
                    } else {
                        print_substep(&format!(
                            "{} All {} attempts failed. Certificate provisioning will need to be completed manually.",
                            "⚠".bright_yellow(),
                            max_retries
                        ));
                        println!();
                    }
                }
            }
        }

        // Step 8: Create DNS A records for routing traffic
        if self.skip_dns_records {
            print_step(8, 8, "DNS A record creation skipped (--skip-dns-records)");
            print_success("Skipping DNS A record creation as requested");
            println!();
        } else {
            print_step(8, 8, "Creating DNS A records for routing traffic");

            // Create wildcard A record (e.g., "*.app" for *.app.example.com)
            let wildcard_record_name = if subdomain_prefix.is_empty() {
                "*".to_string()
            } else {
                format!("*.{}", subdomain_prefix)
            };
            print_substep(&format!(
                "Creating A record: {} → {}",
                format!("{}.{}", wildcard_record_name, base_domain).bright_cyan(),
                server_ip.bright_yellow()
            ));
            if let Err(e) = rt.block_on(cf_provider.set_a_record(
                &base_domain,
                &wildcard_record_name,
                &server_ip,
            )) {
                warn!("Failed to create wildcard A record: {}", e);
                print_substep(&format!(
                    "{} Failed to create wildcard A record: {}",
                    "⚠".bright_yellow(),
                    e
                ));
            } else {
                print_substep(&format!("{} Wildcard A record created", "✓".bright_green()));
            }

            // Create base subdomain A record (e.g., "app" for app.example.com)
            // This allows direct access to the subdomain without wildcard
            if !subdomain_prefix.is_empty() {
                print_substep(&format!(
                    "Creating A record: {} → {}",
                    format!("{}.{}", subdomain_prefix, base_domain).bright_cyan(),
                    server_ip.bright_yellow()
                ));
                if let Err(e) = rt.block_on(cf_provider.set_a_record(
                    &base_domain,
                    &subdomain_prefix,
                    &server_ip,
                )) {
                    warn!("Failed to create subdomain A record: {}", e);
                    print_substep(&format!(
                        "{} Failed to create subdomain A record: {}",
                        "⚠".bright_yellow(),
                        e
                    ));
                } else {
                    print_substep(&format!(
                        "{} Subdomain A record created",
                        "✓".bright_green()
                    ));
                }
            }
            println!();
        }

        // Show warning if certificate wasn't provisioned but continue with setup
        if !certificate_success {
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
            );
            println!(
                "   {} {}",
                "⚠".bright_yellow(),
                "SSL certificate provisioning was not completed.".bright_yellow()
            );
            println!(
                "   {}",
                "You can complete it later via the admin panel.".bright_white()
            );
            println!(
                "   {}",
                "DNS TXT records have been left in place for retry.".bright_white()
            );
            println!(
                "{}",
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
            );
            println!();
        }

        // Update application settings with preview domain and external URL
        print_section("Application Settings");
        let preview_domain = extract_preview_domain(&self.wildcard_domain);
        rt.block_on(update_app_settings(
            db.as_ref(),
            &preview_domain,
            self.external_url.as_deref(),
        ))?;
        print_success(&format!(
            "Preview domain set to: {}",
            preview_domain.bright_cyan()
        ));
        if let Some(ref url) = self.external_url {
            print_success(&format!("External URL set to: {}", url.bright_cyan()));
        }
        println!();

        finish_setup(
            &user,
            &password,
            &self.wildcard_domain,
            self.non_interactive,
            &self.output_format,
        )
    }
}

/// JSON output structure for automation
#[derive(serde::Serialize)]
struct SetupResult {
    success: bool,
    admin_email: String,
    admin_password: Option<String>,
    wildcard_domain: String,
    message: String,
}

fn finish_setup(
    user: &users::Model,
    password: &str,
    wildcard_domain: &str,
    non_interactive: bool,
    output_format: &OutputFormat,
) -> anyhow::Result<()> {
    // JSON output for automation
    if matches!(output_format, OutputFormat::Json) {
        let result = SetupResult {
            success: true,
            admin_email: user.email.clone(),
            admin_password: if password.is_empty() {
                None
            } else {
                Some(password.to_string())
            },
            wildcard_domain: wildcard_domain.to_string(),
            message: "Setup completed successfully".to_string(),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| r#"{"success":true}"#.to_string())
        );
        return Ok(());
    }

    // Text output (default)
    print_section("Setup Complete!");

    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!("{}", "   🎉 Temps is ready to use!".bright_white().bold());
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!();

    println!(
        "{} {}",
        "Wildcard Domain:".bright_white().bold(),
        wildcard_domain.bright_cyan()
    );

    if !password.is_empty() {
        println!(
            "{} {}",
            "Admin Email:".bright_white().bold(),
            user.email.bright_cyan()
        );
        println!(
            "{} {}",
            "Admin Password:".bright_white().bold(),
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
        println!();

        if !non_interactive {
            loop {
                if ask_confirmation("Have you saved the password? (y/n):")? {
                    break;
                }
                println!();
                println!(
                    "{} {}",
                    "Password:".bright_white().bold(),
                    password.bright_yellow().bold()
                );
                println!();
            }
        }
    }

    println!();
    println!("{}", "Next steps:".bright_white().bold());
    println!();
    println!("   {} Start the server:", "1.".bright_cyan());
    println!(
        "      {} temps serve --database-url=<URL>",
        "$".bright_cyan()
    );
    println!();
    println!(
        "   {} Access the admin panel at http://localhost:3000",
        "2.".bright_cyan()
    );
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!();

    debug!("Setup completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dns_provider_type_display() {
        assert_eq!(DnsProviderType::Cloudflare.to_string(), "cloudflare");
        assert_eq!(DnsProviderType::Route53.to_string(), "route53");
        assert_eq!(DnsProviderType::DigitalOcean.to_string(), "digitalocean");
        assert_eq!(DnsProviderType::Azure.to_string(), "azure");
        assert_eq!(DnsProviderType::Gcp.to_string(), "gcp");
    }

    #[test]
    fn test_generate_secure_password() {
        let password = generate_secure_password();
        assert_eq!(password.len(), 16);

        // Ensure it contains valid characters
        let valid_chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
        for c in password.chars() {
            assert!(
                valid_chars.contains(c),
                "Invalid character in password: {}",
                c
            );
        }
    }

    #[test]
    fn test_passwords_are_unique() {
        let password1 = generate_secure_password();
        let password2 = generate_secure_password();
        assert_ne!(password1, password2);
    }
}
