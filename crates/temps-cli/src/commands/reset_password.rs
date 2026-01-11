use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use clap::Args;
use colored::Colorize;
use rand::Rng;
use sea_orm::{ActiveModelTrait, EntityTrait, QueryFilter, Set};
use std::io::{self, Write};
use std::path::PathBuf;
use temps_entities::users;
use tracing::{debug, info};

#[derive(Args)]
pub struct ResetPasswordCommand {
    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
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

async fn reset_admin_password(conn: &sea_orm::DatabaseConnection) -> anyhow::Result<()> {
    use sea_orm::ColumnTrait;

    // Find the admin user (first user with admin role)
    let admin_role = temps_entities::roles::Entity::find()
        .filter(temps_entities::roles::Column::Name.eq("admin"))
        .one(conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Admin role not found"))?;

    let admin_user_role = temps_entities::user_roles::Entity::find()
        .filter(temps_entities::user_roles::Column::RoleId.eq(admin_role.id))
        .one(conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No admin user found"))?;

    let user = users::Entity::find_by_id(admin_user_role.user_id)
        .one(conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Admin user not found"))?;

    // Generate a new secure random password
    let new_password = generate_secure_password();

    // Hash the password using Argon2
    let argon2 = Argon2::default();
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = argon2
        .hash_password(new_password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();

    // Update the user's password
    let mut user_update: users::ActiveModel = user.clone().into();
    user_update.password_hash = Set(Some(password_hash));
    user_update.update(conn).await?;

    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
    );
    println!(
        "{}",
        "   🔑 Admin password reset successfully!"
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
        user.email.bright_cyan()
    );
    println!(
        "{} {}",
        "New Password:".bright_white().bold(),
        new_password.bright_yellow().bold()
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
            println!("{}", "✅ Password reset complete!".bright_green());
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
                "New Password:".bright_white().bold(),
                new_password.bright_yellow().bold()
            );
            println!();
        } else {
            println!(
                "{}",
                "Please enter 'y' for yes or 'n' for no.".bright_white()
            );
        }
    }

    debug!("Reset admin password for user: {}", user.email);

    Ok(())
}

impl ResetPasswordCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        info!("Resetting admin password");

        debug!("Initializing database connection...");
        // Create tokio runtime for database connection
        let rt = tokio::runtime::Runtime::new()?;
        let db = rt.block_on(temps_database::establish_connection(&self.database_url))?;

        // Reset the admin password
        rt.block_on(reset_admin_password(db.as_ref()))?;

        Ok(())
    }
}
