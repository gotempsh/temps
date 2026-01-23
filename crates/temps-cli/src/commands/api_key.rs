//! API Key management command
//!
//! Creates API keys programmatically with a specified role.
//! Useful for automation and CI/CD pipelines.

use clap::Args;
use colored::Colorize;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_auth::{ApiKeyService, CreateApiKeyRequest};
use temps_entities::users;
use tracing::debug;

/// Output format for the API key command
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output with colors and formatting
    #[default]
    Text,
    /// JSON output for automation and scripting
    Json,
}

#[derive(Args)]
pub struct ApiKeyCommand {
    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Name for the API key (for identification)
    #[arg(long)]
    pub name: String,

    /// Role type for the API key
    /// Valid roles: admin, user, reader, mcp, api_reader, demo, custom
    #[arg(long, default_value = "admin")]
    pub role: String,

    /// User email to associate the API key with
    /// If not provided, uses the first admin user
    #[arg(long)]
    pub user_email: Option<String>,

    /// Custom permissions (comma-separated, only used with --role=custom)
    /// Example: --permissions "projects:read,deployments:read,environments:read"
    #[arg(long, value_delimiter = ',')]
    pub permissions: Option<Vec<String>>,

    /// Expiration in days (default: 365 days / 1 year)
    #[arg(long)]
    pub expires_in_days: Option<i64>,

    /// Output format: text (human-readable) or json (machine-readable)
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,
}

impl ApiKeyCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        debug!("Creating API key with role: {}", self.role);

        // Create tokio runtime for database connection
        let rt = tokio::runtime::Runtime::new()?;
        let db = rt.block_on(temps_database::establish_connection(&self.database_url))?;

        // Find the user to associate the API key with
        let user = rt.block_on(async {
            if let Some(email) = &self.user_email {
                // Find user by email
                users::Entity::find()
                    .filter(users::Column::Email.eq(email.to_lowercase()))
                    .filter(users::Column::DeletedAt.is_null())
                    .one(db.as_ref())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("User with email '{}' not found", email))
            } else {
                // Find the first admin user
                let admin_role = temps_entities::roles::Entity::find()
                    .filter(temps_entities::roles::Column::Name.eq("admin"))
                    .one(db.as_ref())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Admin role not found"))?;

                let admin_user_role = temps_entities::user_roles::Entity::find()
                    .filter(temps_entities::user_roles::Column::RoleId.eq(admin_role.id))
                    .one(db.as_ref())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("No admin user found"))?;

                users::Entity::find_by_id(admin_user_role.user_id)
                    .filter(users::Column::DeletedAt.is_null())
                    .one(db.as_ref())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Admin user not found"))
            }
        })?;

        debug!(
            "Creating API key for user: {} (id: {})",
            user.email, user.id
        );

        // Calculate expiration
        let expires_at = self
            .expires_in_days
            .map(|days| chrono::Utc::now() + chrono::Duration::days(days));

        // Create the API key using the service
        let api_key_service = ApiKeyService::new(db.clone());

        let request = CreateApiKeyRequest {
            name: self.name.clone(),
            role_type: self.role.clone(),
            permissions: self.permissions.clone(),
            expires_at,
        };

        let response = rt
            .block_on(api_key_service.create_api_key(user.id, request))
            .map_err(|e| anyhow::anyhow!("Failed to create API key: {}", e))?;

        // Output the result
        match self.output_format {
            OutputFormat::Json => {
                let output = serde_json::json!({
                    "id": response.id,
                    "name": response.name,
                    "api_key": response.api_key,
                    "key_prefix": response.key_prefix,
                    "role_type": response.role_type,
                    "permissions": response.permissions,
                    "user_id": user.id,
                    "user_email": user.email,
                    "expires_at": response.expires_at,
                    "created_at": response.created_at,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            OutputFormat::Text => {
                println!();
                println!(
                    "{}",
                    "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
                );
                println!(
                    "{}",
                    "   🔑 API Key created successfully!".bright_white().bold()
                );
                println!(
                    "{}",
                    "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
                );
                println!();
                println!(
                    "{:>14} {}",
                    "Name:".bright_white().bold(),
                    response.name.bright_cyan()
                );
                println!(
                    "{:>14} {}",
                    "Role:".bright_white().bold(),
                    response.role_type.bright_cyan()
                );
                println!(
                    "{:>14} {}",
                    "User:".bright_white().bold(),
                    user.email.bright_cyan()
                );
                println!(
                    "{:>14} {}",
                    "Key Prefix:".bright_white().bold(),
                    response.key_prefix.bright_cyan()
                );
                if let Some(expires) = response.expires_at {
                    println!(
                        "{:>14} {}",
                        "Expires:".bright_white().bold(),
                        expires
                            .format("%Y-%m-%d %H:%M:%S UTC")
                            .to_string()
                            .bright_cyan()
                    );
                }
                println!();
                println!(
                    "{:>14} {}",
                    "API Key:".bright_white().bold(),
                    response.api_key.bright_yellow().bold()
                );
                println!();
                println!(
                    "{}",
                    "⚠️  IMPORTANT: Save this API key now!"
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
            }
        }

        Ok(())
    }
}
