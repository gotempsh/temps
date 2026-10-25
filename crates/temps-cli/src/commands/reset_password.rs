// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use argon2::{Argon2, PasswordHasher};
use clap::Args;
use colored::Colorize;
use rand::RngExt;
use sea_orm::{ActiveModelTrait, EntityTrait, QueryFilter, QuerySelect, Set, TransactionTrait};
use std::io::{self, Write};
use std::path::PathBuf;
use temps_entities::users;
use tracing::{debug, info};

#[derive(Args)]
pub struct ResetPasswordCommand {
    /// Database connection URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Set the admin password to this exact value instead of generating a
    /// random one. When provided, the command runs non-interactively: no
    /// password is printed and no "have you saved it?" confirmation is
    /// requested. Intended for scripted/CI use.
    #[arg(long, env = "TEMPS_ADMIN_PASSWORD")]
    pub password: Option<String>,

    /// Also remove the admin's multi-factor authentication (TOTP secret and
    /// recovery codes) and sign out all of their sessions, for an admin who
    /// lost both their authenticator and their recovery codes and has no
    /// other admin to reset it for them. If the instance requires MFA for
    /// admins, that requirement is turned off too, since it would otherwise
    /// refuse the admin's password sign-in; turn it back on once they have
    /// enrolled again.
    #[arg(long)]
    pub clear_mfa: bool,
}

/// What `--clear-mfa` changed, for the summary printed to the operator.
struct MfaClearOutcome {
    had_mfa: bool,
    sessions_revoked: u64,
    admin_mfa_requirement_disabled: bool,
}

/// Turn off `require_mfa_for_admins` if it is on, returning whether it was.
///
/// While it is on, password sign-in for an admin without MFA is refused, so
/// clearing the only admin's factor would still leave them locked out.
async fn disable_admin_mfa_requirement<C: sea_orm::ConnectionTrait>(
    conn: &C,
) -> anyhow::Result<bool> {
    let Some(settings) = temps_entities::settings::Entity::find_by_id(1)
        .one(conn)
        .await?
    else {
        return Ok(false);
    };
    if !temps_core::AppSettings::from_json(settings.data.clone()).require_mfa_for_admins {
        return Ok(false);
    }

    let mut data = settings.data.clone();
    let Some(object) = data.as_object_mut() else {
        return Err(anyhow::anyhow!(
            "Settings row 1 is not a JSON object; cannot turn off require_mfa_for_admins"
        ));
    };
    object.insert(
        "require_mfa_for_admins".to_string(),
        serde_json::Value::Bool(false),
    );
    let mut update: temps_entities::settings::ActiveModel = settings.into();
    update.data = Set(data);
    update.update(conn).await?;
    Ok(true)
}

/// Set `user_id`'s password hash and, with `clear_mfa`, remove their second
/// factor, in one transaction.
async fn apply_admin_reset(
    conn: &sea_orm::DatabaseConnection,
    user_id: i32,
    password_hash: String,
    clear_mfa: bool,
) -> anyhow::Result<Option<MfaClearOutcome>> {
    let transaction = conn.begin().await?;
    // Same row lock every second-factor check takes.
    let user = users::Entity::find_by_id(user_id)
        .lock_exclusive()
        .one(&transaction)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Admin user {} not found", user_id))?;

    let mut user_update: users::ActiveModel = user.into();
    user_update.password_hash = Set(Some(password_hash));
    let user = user_update.update(&transaction).await?;

    let outcome = if clear_mfa {
        let had_mfa = user.mfa_enabled;
        let cleared = temps_auth::clear_mfa_enrollment(&transaction, user).await?;
        let admin_mfa_requirement_disabled = disable_admin_mfa_requirement(&transaction).await?;
        Some(MfaClearOutcome {
            had_mfa,
            sessions_revoked: cleared.sessions_revoked,
            admin_mfa_requirement_disabled,
        })
    } else {
        None
    };

    transaction.commit().await?;
    Ok(outcome)
}

fn print_mfa_outcome(outcome: &MfaClearOutcome) {
    if outcome.had_mfa {
        println!(
            "{}",
            "Multi-factor authentication removed: sign in with the password alone, then enroll again from your account settings."
                .bright_white()
        );
    } else {
        println!(
            "{}",
            "This admin had no multi-factor authentication enrolled; nothing to remove."
                .bright_white()
        );
    }
    println!(
        "{} {}",
        "Sessions signed out:".bright_white().bold(),
        outcome.sessions_revoked
    );
    if outcome.admin_mfa_requirement_disabled {
        println!(
            "{}",
            "⚠️  \"Require MFA for admins\" was on and has been turned off, otherwise this admin could not sign in without MFA. Turn it back on in Settings once every admin has enrolled again."
                .bright_yellow()
                .bold()
        );
    }
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

async fn reset_admin_password(
    conn: &sea_orm::DatabaseConnection,
    provided_password: Option<String>,
    clear_mfa: bool,
) -> anyhow::Result<()> {
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

    // When a password is supplied, run non-interactively. Otherwise generate
    // a random one and walk the operator through saving it.
    let non_interactive = provided_password.is_some();
    let new_password = match provided_password {
        Some(pw) => {
            if pw.is_empty() {
                return Err(anyhow::anyhow!("Provided password cannot be empty"));
            }
            pw
        }
        None => generate_secure_password(),
    };

    // Hash the password using Argon2
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(new_password.as_bytes())
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();

    let mfa_outcome = apply_admin_reset(conn, user.id, password_hash, clear_mfa).await?;
    if let Some(outcome) = &mfa_outcome {
        info!(
            "Cleared MFA for admin user {} ({} session(s) revoked, admin MFA requirement disabled: {})",
            user.id, outcome.sessions_revoked, outcome.admin_mfa_requirement_disabled
        );
    }

    // Non-interactive path: confirm success without echoing the password
    // (the caller already knows it) and skip the save-confirmation prompt.
    if non_interactive {
        println!();
        println!(
            "{}",
            "✅ Admin password reset successfully (non-interactive).".bright_green()
        );
        println!(
            "{} {}",
            "Email:".bright_white().bold(),
            user.email.bright_cyan()
        );
        if let Some(outcome) = &mfa_outcome {
            print_mfa_outcome(outcome);
        }
        println!();
        debug!("Reset admin password for user: {}", user.email);
        return Ok(());
    }

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
    if let Some(outcome) = &mfa_outcome {
        print_mfa_outcome(outcome);
    }
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
        rt.block_on(reset_admin_password(
            db.as_ref(),
            self.password,
            self.clear_mfa,
        ))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::DatabaseConnection;

    async fn test_db() -> Option<temps_database::test_utils::TestDatabase> {
        match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker not available, skipping test: {error}");
                None
            }
            Err(error) => panic!("test database setup failed: {error}"),
        }
    }

    /// An admin with MFA enrolled, one signed-in session, a second-factor
    /// lockout, and an instance that requires MFA for admins.
    async fn locked_out_admin(db: &DatabaseConnection) -> users::Model {
        let user = users::ActiveModel {
            email: Set("admin@example.com".to_string()),
            name: Set("Admin".to_string()),
            email_verified: Set(true),
            mfa_enabled: Set(true),
            mfa_secret: Set(Some("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP".to_string())),
            mfa_recovery_codes: Set(Some("[\"hash\"]".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        temps_entities::sessions::ActiveModel {
            user_id: Set(user.id),
            session_token: Set("old-session".to_string()),
            expires_at: Set(Utc::now() + chrono::Duration::days(1)),
            mfa_pending: Set(false),
            mfa_failed_attempts: Set(0),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        temps_entities::user_mfa_guards::ActiveModel {
            user_id: Set(user.id),
            last_totp_step: Set(Some(1)),
            failed_attempts: Set(10),
            locked_until: Set(Some(Utc::now() + chrono::Duration::hours(1))),
            updated_at: Set(Utc::now()),
        }
        .insert(db)
        .await
        .unwrap();
        temps_entities::settings::ActiveModel {
            id: Set(1),
            data: Set(serde_json::json!({
                "external_url": "https://temps.example.com",
                "require_mfa_for_admins": true,
            })),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        user
    }

    async fn settings_data(db: &DatabaseConnection) -> serde_json::Value {
        temps_entities::settings::Entity::find_by_id(1)
            .one(db)
            .await
            .unwrap()
            .unwrap()
            .data
    }

    /// Issue #1590: the offline reset changed only the password, so a sole
    /// admin who lost their authenticator and recovery codes stayed locked
    /// out.
    #[tokio::test]
    async fn clear_mfa_unlocks_an_admin_who_lost_every_factor() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.as_ref();
        let admin = locked_out_admin(db).await;

        let outcome = apply_admin_reset(db, admin.id, "new-hash".to_string(), true)
            .await
            .unwrap()
            .expect("--clear-mfa reports what it changed");
        assert!(outcome.had_mfa);
        assert_eq!(outcome.sessions_revoked, 1);
        assert!(outcome.admin_mfa_requirement_disabled);

        let stored = users::Entity::find_by_id(admin.id)
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.password_hash.as_deref(), Some("new-hash"));
        assert!(!stored.mfa_enabled);
        assert!(stored.mfa_secret.is_none());
        assert!(stored.mfa_recovery_codes.is_none());
        assert!(
            temps_entities::user_mfa_guards::Entity::find_by_id(admin.id)
                .one(db)
                .await
                .unwrap()
                .is_none()
        );
        assert!(temps_entities::sessions::Entity::find()
            .all(db)
            .await
            .unwrap()
            .is_empty());

        let settings = settings_data(db).await;
        assert_eq!(settings["require_mfa_for_admins"], false);
        assert_eq!(
            settings["external_url"], "https://temps.example.com",
            "other settings are kept"
        );
    }

    #[tokio::test]
    async fn without_clear_mfa_only_the_password_changes() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.as_ref();
        let admin = locked_out_admin(db).await;

        let outcome = apply_admin_reset(db, admin.id, "new-hash".to_string(), false)
            .await
            .unwrap();
        assert!(outcome.is_none());

        let stored = users::Entity::find_by_id(admin.id)
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.password_hash.as_deref(), Some("new-hash"));
        assert!(stored.mfa_enabled);
        assert!(stored.mfa_secret.is_some());
        assert_eq!(
            temps_entities::sessions::Entity::find()
                .all(db)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(settings_data(db).await["require_mfa_for_admins"], true);
    }
}
