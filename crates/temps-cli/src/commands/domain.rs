//! Domain management commands for certificate import and management

use anyhow::Context;
use chrono::Utc;
use clap::{Args, Subcommand};
use colored::Colorize;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::fs;
use std::path::PathBuf;
use temps_core::EncryptionService;
use temps_database::establish_connection;
use temps_entities::domains;
use tracing::debug;
use x509_parser::prelude::*;

/// Domain and certificate management commands
#[derive(Args)]
pub struct DomainCommand {
    #[command(subcommand)]
    pub command: DomainSubcommand,
}

#[derive(Subcommand)]
pub enum DomainSubcommand {
    /// Import a custom certificate for a domain
    Import(ImportCertificateCommand),
    /// List all domains and their certificate status
    List(ListDomainsCommand),
}

/// Import a custom certificate for a domain
#[derive(Args)]
pub struct ImportCertificateCommand {
    /// Domain name (e.g., "*.localho.st" or "app.example.com")
    #[arg(long, short = 'd')]
    pub domain: String,

    /// Path to the certificate file (PEM format)
    #[arg(long, short = 'c')]
    pub certificate: PathBuf,

    /// Path to the private key file (PEM format)
    #[arg(long, short = 'k')]
    pub private_key: PathBuf,

    /// Database URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Data directory containing the encryption key
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Overwrite existing certificate for this domain
    #[arg(long, default_value = "false")]
    pub force: bool,
}

/// List all domains and their certificate status
#[derive(Args)]
pub struct ListDomainsCommand {
    /// Database URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,
}

impl DomainCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;

        rt.block_on(async {
            match self.command {
                DomainSubcommand::Import(cmd) => execute_import(cmd).await,
                DomainSubcommand::List(cmd) => execute_list(cmd).await,
            }
        })
    }
}

async fn execute_import(cmd: ImportCertificateCommand) -> anyhow::Result<()> {
    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_blue()
    );
    println!(
        "{}",
        "                  Import Custom Certificate                     "
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_blue()
    );
    println!();

    // Get data directory
    let data_dir = get_data_dir(&cmd.data_dir)?;
    debug!("Using data directory: {}", data_dir.display());

    // Load encryption key
    let encryption_key = load_encryption_key(&data_dir)?;
    let encryption_service = EncryptionService::new(&encryption_key)
        .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?;

    // Read certificate and private key files
    println!(
        "{} Reading certificate from: {}",
        "→".bright_blue(),
        cmd.certificate.display()
    );
    let certificate_pem = fs::read_to_string(&cmd.certificate).with_context(|| {
        format!(
            "Failed to read certificate file: {}",
            cmd.certificate.display()
        )
    })?;

    println!(
        "{} Reading private key from: {}",
        "→".bright_blue(),
        cmd.private_key.display()
    );
    let private_key_pem = fs::read_to_string(&cmd.private_key).with_context(|| {
        format!(
            "Failed to read private key file: {}",
            cmd.private_key.display()
        )
    })?;

    // Validate certificate format and extract expiration
    let expiration_time = validate_and_parse_certificate(&certificate_pem, &cmd.domain)?;

    // Validate private key format
    validate_private_key(&private_key_pem)?;

    // Encrypt the private key
    println!("{} Encrypting private key...", "→".bright_blue());
    let encrypted_private_key = encryption_service
        .encrypt_string(&private_key_pem)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt private key: {}", e))?;

    // Connect to database
    println!("{} Connecting to database...", "→".bright_blue());
    let db = establish_connection(&cmd.database_url).await?;

    // Check if domain already exists
    let existing = domains::Entity::find()
        .filter(domains::Column::Domain.eq(&cmd.domain))
        .one(db.as_ref())
        .await?;

    let is_wildcard = cmd.domain.starts_with("*.");

    if let Some(existing_domain) = existing {
        if !cmd.force {
            return Err(anyhow::anyhow!(
                "Domain '{}' already exists. Use --force to overwrite.",
                cmd.domain
            ));
        }

        println!(
            "{} Updating existing domain certificate...",
            "→".bright_yellow()
        );

        // Update existing domain
        let mut domain_update: domains::ActiveModel = existing_domain.into();
        domain_update.certificate = Set(Some(certificate_pem.clone()));
        domain_update.private_key = Set(Some(encrypted_private_key));
        domain_update.expiration_time = Set(Some(expiration_time));
        domain_update.status = Set("active".to_string());
        domain_update.last_renewed = Set(Some(Utc::now()));
        domain_update.last_error = Set(None);
        domain_update.last_error_type = Set(None);
        domain_update.verification_method = Set("manual".to_string());
        domain_update.updated_at = Set(Utc::now());

        domain_update.update(db.as_ref()).await?;
    } else {
        println!(
            "{} Creating new domain with certificate...",
            "→".bright_blue()
        );

        // Create new domain
        let new_domain = domains::ActiveModel {
            domain: Set(cmd.domain.clone()),
            certificate: Set(Some(certificate_pem.clone())),
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

        new_domain.insert(db.as_ref()).await?;
    }

    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_green()
    );
    println!("{} Certificate imported successfully!", "✓".bright_green());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_green()
    );
    println!();
    println!(
        "  {} {}",
        "Domain:".bright_white(),
        cmd.domain.bright_cyan()
    );
    println!(
        "  {} {}",
        "Type:".bright_white(),
        if is_wildcard {
            "Wildcard"
        } else {
            "Single domain"
        }
        .bright_cyan()
    );
    println!(
        "  {} {}",
        "Expires:".bright_white(),
        expiration_time
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string()
            .bright_cyan()
    );
    println!("  {} {}", "Status:".bright_white(), "active".bright_green());
    println!();

    Ok(())
}

async fn execute_list(cmd: ListDomainsCommand) -> anyhow::Result<()> {
    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_blue()
    );
    println!(
        "{}",
        "                      Domain Certificates                       "
            .bright_blue()
            .bold()
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".bright_blue()
    );
    println!();

    // Connect to database
    let db = establish_connection(&cmd.database_url).await?;

    // List all domains
    let domains_list = domains::Entity::find().all(db.as_ref()).await?;

    if domains_list.is_empty() {
        println!("  {} No domains configured.", "ℹ".bright_blue());
        println!();
        return Ok(());
    }

    println!(
        "  {:<40} {:<15} {:<12} {:<20}",
        "DOMAIN".bright_white().bold(),
        "STATUS".bright_white().bold(),
        "TYPE".bright_white().bold(),
        "EXPIRES".bright_white().bold()
    );
    println!("  {}", "─".repeat(90));

    for domain in domains_list {
        let status_colored = match domain.status.as_str() {
            "active" => domain.status.bright_green(),
            "pending" | "pending_dns" | "pending_validation" | "pending_http" => {
                domain.status.bright_yellow()
            }
            "failed" | "expired" => domain.status.bright_red(),
            _ => domain.status.normal(),
        };

        let domain_type = if domain.is_wildcard {
            "wildcard"
        } else {
            "single"
        };

        let expiration = domain
            .expiration_time
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "N/A".to_string());

        println!(
            "  {:<40} {:<15} {:<12} {:<20}",
            domain.domain.bright_cyan(),
            status_colored,
            domain_type,
            expiration
        );
    }

    println!();
    Ok(())
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

fn load_encryption_key(data_dir: &PathBuf) -> anyhow::Result<String> {
    let encryption_key_path = data_dir.join("encryption_key");

    if !encryption_key_path.exists() {
        return Err(anyhow::anyhow!(
            "Encryption key not found at {}. Run 'temps setup' first to initialize the data directory.",
            encryption_key_path.display()
        ));
    }

    let key = fs::read_to_string(&encryption_key_path)
        .map_err(|e| anyhow::anyhow!("Failed to read encryption key: {}", e))?;

    Ok(key.trim().to_string())
}

fn validate_and_parse_certificate(
    cert_pem: &str,
    expected_domain: &str,
) -> anyhow::Result<chrono::DateTime<Utc>> {
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
            "{} Certificate domains: {:?}",
            "⚠".bright_yellow(),
            cert_domains
        );
        println!(
            "{} Expected domain '{}' does not match certificate. Proceeding anyway...",
            "⚠".bright_yellow(),
            expected_domain
        );
    } else {
        println!("{} Certificate domain validated", "✓".bright_green());
    }

    println!(
        "{} Certificate expires: {}",
        "✓".bright_green(),
        expiration_time.format("%Y-%m-%d %H:%M:%S UTC")
    );

    Ok(expiration_time)
}

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

    println!("{} Private key format validated", "✓".bright_green());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_private_key_rsa() {
        let rsa_key = r#"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHB7MxszF8Pf0Q3/Y
-----END RSA PRIVATE KEY-----"#;

        assert!(validate_private_key(rsa_key).is_ok());
    }

    #[test]
    fn test_validate_private_key_pkcs8() {
        let pkcs8_key = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDRndVLkklx2zfF
-----END PRIVATE KEY-----"#;

        assert!(validate_private_key(pkcs8_key).is_ok());
    }

    #[test]
    fn test_validate_private_key_ec() {
        let ec_key = r#"-----BEGIN EC PRIVATE KEY-----
MHQCAQEEIBDl5iLbSt9+cjO0XBcY7TPLYJ1YK/sFsYl1qVRkuVQLoAcGBSuBBAAK
-----END EC PRIVATE KEY-----"#;

        assert!(validate_private_key(ec_key).is_ok());
    }

    #[test]
    fn test_validate_private_key_invalid() {
        let invalid_key = "not a valid key";
        assert!(validate_private_key(invalid_key).is_err());
    }

    #[test]
    fn test_validate_private_key_wrong_type() {
        let wrong_type = r#"-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJAKHBfpeg...
-----END CERTIFICATE-----"#;

        assert!(validate_private_key(wrong_type).is_err());
    }
}
