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
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use temps_core::EncryptionService;
use temps_dns::providers::credentials::{
    AzureCredentials, CloudflareCredentials, DigitalOceanCredentials, GcpCredentials,
    ProviderCredentials, Route53Credentials,
};
use temps_domains::dns_provider::CloudflareDnsProvider;
use temps_entities::{dns_providers, domains, git_providers, roles, user_roles, users};
use tracing::{debug, info};

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

    /// Domain pattern for SSL certificate (e.g., "*.app.example.com")
    #[arg(long)]
    pub domain: Option<String>,

    /// DNS provider type
    #[arg(long, value_enum)]
    pub dns_provider: Option<DnsProviderType>,

    /// Cloudflare API token (for Cloudflare DNS provider)
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

    /// GitHub Personal Access Token
    #[arg(long, env = "GITHUB_TOKEN")]
    pub github_token: Option<String>,

    /// Skip interactive prompts and use defaults
    #[arg(long, default_value = "false")]
    pub non_interactive: bool,
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

async fn create_admin_user(
    conn: &sea_orm::DatabaseConnection,
    email: &str,
) -> anyhow::Result<(users::Model, String)> {
    let email_lower = email.to_lowercase();

    // Check if user exists
    let existing_user = users::Entity::find()
        .filter(users::Column::Email.eq(&email_lower))
        .one(conn)
        .await?;

    if existing_user.is_some() {
        return Err(anyhow::anyhow!(
            "User with email {} already exists. Use 'temps reset-admin-password' to reset the password.",
            email_lower
        ));
    }

    // Generate secure password
    let password = generate_secure_password();

    // Hash password with Argon2
    let argon2 = Argon2::default();
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();

    // Create user
    let new_user = users::ActiveModel {
        email: Set(email_lower.clone()),
        name: Set("Admin".to_string()),
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
    Ok((user, password))
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
        info!("DNS provider '{}' already exists", provider_type_str);
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

async fn create_git_provider(
    conn: &sea_orm::DatabaseConnection,
    encryption_service: &EncryptionService,
    token: &str,
) -> anyhow::Result<git_providers::Model> {
    // Check if GitHub provider already exists
    let existing = git_providers::Entity::find()
        .filter(git_providers::Column::ProviderType.eq("github"))
        .filter(git_providers::Column::AuthMethod.eq("pat"))
        .one(conn)
        .await?;

    if let Some(provider) = existing {
        info!("GitHub provider already exists");
        return Ok(provider);
    }

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
    Ok(provider)
}

async fn create_domain(
    conn: &sea_orm::DatabaseConnection,
    domain_name: &str,
) -> anyhow::Result<domains::Model> {
    // Check if domain already exists
    let existing = domains::Entity::find()
        .filter(domains::Column::Domain.eq(domain_name))
        .one(conn)
        .await?;

    if let Some(domain) = existing {
        info!("Domain '{}' already exists", domain_name);
        return Ok(domain);
    }

    let is_wildcard = domain_name.starts_with("*.");

    // Create domain record (using dns-01 for wildcard domains, http-01 otherwise)
    let verification_method = if is_wildcard { "dns-01" } else { "http-01" };

    let new_domain = domains::ActiveModel {
        domain: Set(domain_name.to_string()),
        status: Set("pending".to_string()),
        is_wildcard: Set(is_wildcard),
        verification_method: Set(verification_method.to_string()),
        dns_challenge_token: Set(None),
        dns_challenge_value: Set(None),
        http_challenge_token: Set(None),
        http_challenge_key_authorization: Set(None),
        certificate: Set(None),
        private_key: Set(None),
        expiration_time: Set(None),
        last_renewed: Set(None),
        last_error: Set(None),
        last_error_type: Set(None),
        ..Default::default()
    };

    let domain = new_domain.insert(conn).await?;
    debug!(
        "Created domain '{}' with {} verification",
        domain_name, verification_method
    );
    Ok(domain)
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

async fn verify_github_token(token: &str) -> anyhow::Result<bool> {
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
        Ok(true)
    } else {
        Err(anyhow::anyhow!(
            "GitHub token is invalid or does not have required permissions (status: {})",
            response.status()
        ))
    }
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

fn ask_confirmation(prompt: &str) -> anyhow::Result<bool> {
    print!("{} ", prompt.bright_white().bold());
    io::stdout().flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    let response = response.trim().to_lowercase();

    Ok(response == "y" || response == "yes")
}

impl SetupCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        print_header();

        info!("Starting Temps setup");

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

        // Create tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new()?;

        // Establish database connection (this also runs migrations)
        print_section("Database Setup");
        println!("   Connecting to database and running migrations...");

        let db = rt.block_on(temps_database::establish_connection(&self.database_url))?;
        print_success("Database connected and migrations applied");

        // Create admin user
        print_section("Admin User Setup");
        let (user, password) = match rt.block_on(create_admin_user(db.as_ref(), &self.admin_email))
        {
            Ok((user, password)) => {
                print_success("Admin user created");
                (user, password)
            }
            Err(e) => {
                if e.to_string().contains("already exists") {
                    print_warning(&e.to_string());
                    println!();
                    // Continue with setup even if user exists
                    (
                        users::Model {
                            id: 0,
                            email: self.admin_email.clone(),
                            name: "Admin".to_string(),
                            password_hash: None,
                            email_verified: true,
                            mfa_enabled: false,
                            mfa_secret: None,
                            mfa_recovery_codes: None,
                            deleted_at: None,
                            email_verification_token: None,
                            email_verification_expires: None,
                            password_reset_token: None,
                            password_reset_expires: None,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                        },
                        String::new(),
                    )
                } else {
                    return Err(e);
                }
            }
        };

        // DNS Provider setup
        if let Some(ref provider_type) = self.dns_provider {
            print_section("DNS Provider Setup");

            let credentials = match provider_type {
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

            rt.block_on(create_dns_provider(
                db.as_ref(),
                &encryption_service,
                provider_type,
                credentials,
            ))?;
            print_success(&format!("{} DNS provider configured", provider_type));
        }

        // GitHub Provider setup
        if let Some(ref token) = self.github_token {
            print_section("Git Provider Setup");

            println!("   Verifying GitHub token...");
            rt.block_on(verify_github_token(token))?;
            print_success("GitHub token verified");

            rt.block_on(create_git_provider(db.as_ref(), &encryption_service, token))?;
            print_success("GitHub provider configured");
        }

        // Domain setup
        if let Some(ref domain_name) = self.domain {
            print_section("Domain Setup");

            rt.block_on(create_domain(db.as_ref(), domain_name))?;

            let is_wildcard = domain_name.starts_with("*.");
            print_success(&format!("Domain '{}' created", domain_name));

            if is_wildcard {
                println!();
                print_warning("Wildcard domains require DNS-01 challenge validation.");
                println!(
                    "   After starting the server, use the API to request and complete the challenge:"
                );
                println!();
                println!(
                    "   {} POST /api/domains/{}/order",
                    "1.".bright_cyan(),
                    "domain_id"
                );
                println!("   {} Add TXT record to DNS", "2.".bright_cyan());
                println!(
                    "   {} POST /api/domains/{}/finalize",
                    "3.".bright_cyan(),
                    "domain_id"
                );
            }
        }

        // Print summary
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

            if !self.non_interactive {
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

        if self.domain.is_some()
            && self
                .domain
                .as_ref()
                .map(|d| d.starts_with("*."))
                .unwrap_or(false)
        {
            println!(
                "   {} Complete SSL certificate provisioning via the API",
                "2.".bright_cyan()
            );
            println!();
        }

        println!(
            "   {} Access the admin panel at http://localhost:3000",
            if self.domain.is_some()
                && self
                    .domain
                    .as_ref()
                    .map(|d| d.starts_with("*."))
                    .unwrap_or(false)
            {
                "3."
            } else {
                "2."
            }
            .bright_cyan()
        );
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!();

        info!("Setup completed successfully");
        Ok(())
    }
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
