// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-shot host credential adoption for a single-owner installation.
//! The durable marker intentionally has no provider/user foreign key: deleting
//! either must never make a later restart re-adopt the machine credential.
use super::{
    git_provider::{AuthMethod, GitProviderFactory, GitProviderType},
    git_provider_manager::GitProviderManager,
};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Set,
    Statement, TransactionTrait,
};
use std::{process::Stdio, sync::Arc, time::Duration};
use temps_core::EncryptionService;
use temps_entities::{audit_logs, git_provider_connections, git_providers};
use tokio::{io::AsyncReadExt, process::Command};

#[derive(Debug, thiserror::Error)]
pub enum HostImportError {
    #[error("Host Git bootstrap database operation {operation} failed: {source}")]
    Database {
        operation: &'static str,
        source: sea_orm::DbErr,
    },
    #[error("Host Git bootstrap for {provider} failed credential validation; reconnect through Git providers")]
    Validation { provider: &'static str },
    #[error("Host Git bootstrap for {provider} could not encrypt its credential")]
    Encryption { provider: &'static str },
}
fn database(operation: &'static str, source: sea_orm::DbErr) -> HostImportError {
    HostImportError::Database { operation, source }
}

#[derive(Clone, Copy)]
enum Provider {
    Github,
    Gitlab,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }
    fn host(self) -> &'static str {
        match self {
            Self::Github => "github.com",
            Self::Gitlab => "gitlab.com",
        }
    }
    fn kind(self) -> GitProviderType {
        match self {
            Self::Github => GitProviderType::GitHub,
            Self::Gitlab => GitProviderType::GitLab,
        }
    }
    fn variables(self) -> &'static [&'static str] {
        match self {
            Self::Github => &["GH_TOKEN", "GITHUB_TOKEN"],
            Self::Gitlab => &["GITLAB_TOKEN", "GITLAB_ACCESS_TOKEN", "GLAB_TOKEN"],
        }
    }
    fn command(self) -> &'static str {
        match self {
            Self::Github => "gh",
            Self::Gitlab => "glab",
        }
    }
}
fn first_token(names: &[&str], read: impl Fn(&str) -> Option<String>) -> Option<String> {
    names
        .iter()
        .filter_map(|name| read(name))
        .find_map(|value| {
            let value = value.trim();
            (!value.is_empty()
                && value.len() <= 16384
                && value.bytes().all(|byte| (33..=126).contains(&byte)))
            .then(|| value.to_owned())
        })
}
async fn discover(provider: Provider) -> Option<String> {
    // Non-public host overrides must never send an enterprise token to a public host.
    let host_var = match provider {
        Provider::Github => "GH_HOST",
        Provider::Gitlab => "GITLAB_HOST",
    };
    if std::env::var(host_var).is_ok_and(|host| host != provider.host()) {
        return None;
    }
    if let Some(token) = first_token(provider.variables(), |key| std::env::var(key).ok()) {
        return Some(token);
    }
    let args: &[&str] = match provider {
        Provider::Github => &["auth", "token", "--hostname", "github.com"],
        Provider::Gitlab => &["config", "get", "token", "--host", "gitlab.com"],
    };
    let mut child = Command::new(provider.command())
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GLAB_CHECK_UPDATE", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    // Bound both runtime and captured bytes. No CLI diagnostics can contain a token.
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        stdout.take(16385).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 16384 {
            return None;
        }
        if !child.wait().await.ok()?.success() {
            return None;
        }
        let token = String::from_utf8(bytes).ok()?;
        first_token(&["cli"], |_| Some(token.clone()))
    })
    .await;
    result.ok().flatten()
}

/// Called once during startup after user initialization, never from an HTTP request.
/// Only an installation with exactly one non-system user (including deleted users)
/// can adopt credentials. This prevents reassignment when an owner is replaced.
pub async fn import_host_credentials(
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    manager: Arc<GitProviderManager>,
) -> Result<(), HostImportError> {
    let Some(owner) = eligible_owner(db.as_ref()).await? else {
        tracing::info!(
            event = "host_git_import_skipped",
            reason = "requires_single_active_admin_owner"
        );
        return Ok(());
    };
    for provider in [Provider::Github, Provider::Gitlab] {
        if let Err(error) = import_provider(
            db.clone(),
            encryption.clone(),
            manager.clone(),
            owner,
            provider,
        )
        .await
        {
            // Our errors never retain upstream response bodies or credentials.
            tracing::warn!(event="host_git_import_failed", provider=provider.name(), error=%error);
        }
    }
    Ok(())
}
async fn eligible_owner<C: ConnectionTrait>(db: &C) -> Result<Option<i32>, HostImportError> {
    let row = db.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT u.id FROM users u WHERE u.id <> 0 AND u.deleted_at IS NULL AND (SELECT COUNT(*) FROM users WHERE id <> 0) = 1 AND EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE ur.user_id = u.id AND r.name = 'admin')".to_owned())).await.map_err(|e| database("find eligible owner", e))?;
    row.map(|row| {
        row.try_get("", "id")
            .map_err(|e| database("decode eligible owner", e))
    })
    .transpose()
}
async fn already_imported<C: ConnectionTrait>(
    db: &C,
    provider: Provider,
) -> Result<bool, HostImportError> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT provider FROM host_git_imports WHERE provider = $1",
            [provider.name().into()],
        ))
        .await
        .map_err(|e| database("read import marker", e))?
        .is_some())
}
async fn import_provider(
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    manager: Arc<GitProviderManager>,
    owner: i32,
    provider: Provider,
) -> Result<(), HostImportError> {
    let txn = db.begin().await.map_err(|e| database("begin import", e))?;
    // Serializes parallel boots; transaction ends before repository synchronization.
    txn.execute(Statement::from_string(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(82491307)".to_owned(),
    ))
    .await
    .map_err(|e| database("lock import", e))?;
    if already_imported(&txn, provider).await? {
        return Ok(());
    }
    if eligible_owner(&txn).await? != Some(owner) {
        return Ok(());
    }
    let Some(token) = discover(provider).await else {
        return Ok(());
    };
    let base = format!("https://{}", provider.host());
    let api = match provider {
        Provider::Github => "https://api.github.com".to_owned(),
        Provider::Gitlab => format!("{base}/api/v4"),
    };
    let auth = AuthMethod::PersonalAccessToken {
        token: token.clone(),
    };
    let service = GitProviderFactory::create_provider(
        provider.kind(),
        auth,
        Some(base.clone()),
        Some(api.clone()),
        db.clone(),
    )
    .await
    .map_err(|_| HostImportError::Validation {
        provider: provider.name(),
    })?;
    let user = tokio::time::timeout(Duration::from_secs(10), service.get_user(&token))
        .await
        .map_err(|_| HostImportError::Validation {
            provider: provider.name(),
        })?
        .map_err(|_| HostImportError::Validation {
            provider: provider.name(),
        })?;
    // Serialize the final ownership check against concurrent user/role changes.
    txn.execute_unprepared("LOCK TABLE users, user_roles, roles IN SHARE MODE")
        .await
        .map_err(|e| database("lock ownership", e))?;
    if eligible_owner(&txn).await? != Some(owner) {
        return Ok(());
    }
    let connection_id = persist_import(
        &txn,
        &encryption,
        owner,
        provider,
        user.username,
        &token,
        base,
        api,
    )
    .await?;
    txn.commit()
        .await
        .map_err(|e| database("commit import", e))?;
    if let Some(id) = connection_id {
        if manager.spawn_sync_repositories(id).await.is_err() {
            tracing::warn!(
                event = "host_git_sync_not_started",
                connection_id = id,
                message = "Retry repository synchronization in Git providers"
            );
        }
    }
    tracing::info!(
        event = "host_git_import_completed",
        provider = provider.name(),
        user_id = owner
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_import(
    txn: &DatabaseTransaction,
    encryption: &EncryptionService,
    owner: i32,
    provider: Provider,
    username: String,
    token: &str,
    base: String,
    api: String,
) -> Result<Option<i32>, HostImportError> {
    let existing = txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT c.id FROM git_provider_connections c JOIN git_providers p ON p.id = c.provider_id WHERE c.user_id = $1 AND c.account_name = $2 AND p.provider_type = $3 AND p.base_url = $4",
        vec![owner.into(), username.clone().into(), provider.name().into(), base.clone().into()])).await.map_err(|e| database("find existing connection", e))?;
    let mut connection_id = None;
    if existing.is_none() {
        let encrypted =
            encryption
                .encrypt_string(token)
                .map_err(|_| HostImportError::Encryption {
                    provider: provider.name(),
                })?;
        // Provider configuration contains no credential. All authenticated operations
        // use the encrypted per-owner connection token, as OAuth connections do.
        let provider_row = git_providers::ActiveModel {
            name: Set(format!("{} (host)", provider.name())),
            provider_type: Set(provider.name().to_owned()),
            base_url: Set(Some(base)),
            api_url: Set(Some(api)),
            auth_method: Set("pat".to_owned()),
            auth_config: Set(serde_json::json!({"PersonalAccessToken":{"token":""}})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        }
        .insert(txn)
        .await
        .map_err(|e| database("insert provider", e))?;
        let connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider_row.id),
            user_id: Set(Some(owner)),
            account_name: Set(username),
            account_type: Set("User".to_owned()),
            access_token: Set(Some(encrypted)),
            is_active: Set(true),
            metadata: Set(Some(serde_json::json!({"source":"host_import"}))),
            ..Default::default()
        }
        .insert(txn)
        .await
        .map_err(|e| database("insert connection", e))?;
        connection_id = Some(connection.id);
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO host_git_imports (provider, user_id) VALUES ($1, $2)",
        vec![provider.name().into(), owner.into()],
    ))
    .await
    .map_err(|e| database("record import marker", e))?;
    audit_logs::ActiveModel {
        user_id: Set(Some(owner)), user_agent: Set("temps-host-bootstrap".into()), operation_type: Set("HOST_GIT_PROVIDER_IMPORTED".into()), audit_date: Set(chrono::Utc::now()),
        data: Set(serde_json::json!({"provider":provider.name(), "connection_id":connection_id, "existing_connection":existing.is_some()}).to_string()), ..Default::default()
    }.insert(txn).await.map_err(|e| database("audit import", e))?;
    Ok(connection_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_precedence_and_empty_values() {
        assert_eq!(
            first_token(&["GH_TOKEN", "GITHUB_TOKEN"], |key| Some(
                if key == "GH_TOKEN" {
                    " "
                } else {
                    " synthetic "
                }
                .into()
            )),
            Some("synthetic".into())
        );
        assert_eq!(
            first_token(&["first", "second"], |key| Some(key.into())),
            Some("first".into())
        );
        assert_eq!(first_token(&["missing"], |_| None), None);
    }
    #[test]
    fn rejects_multiline_and_oversized_values() {
        assert!(first_token(&["x"], |_| Some("one\ntwo".into())).is_none());
        assert!(first_token(&["x"], |_| Some("x".repeat(16385))).is_none());
    }
    #[test]
    fn public_hosts_and_commands_are_fixed() {
        assert_eq!(Provider::Github.host(), "github.com");
        assert_eq!(Provider::Gitlab.host(), "gitlab.com");
        assert_eq!(Provider::Github.command(), "gh");
        assert_eq!(Provider::Gitlab.command(), "glab");
    }
    #[test]
    fn rejects_control_bytes_before_provider_header_construction() {
        for value in [
            "bad\u{7}token",
            "bad\ttoken",
            "bad token",
            "bad\u{7f}token",
            "nonasciié",
        ] {
            assert!(first_token(&["test"], |_| Some(value.into())).is_none());
        }
    }

    #[tokio::test]
    async fn bootstrap_persistence_and_owner_guards() {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::{roles, user_roles, users};
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Docker unavailable; skipping host import integration test: {error}");
                return;
            }
            Err(error) => panic!("Host import test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let owner = users::ActiveModel {
            email: Set("bootstrap@example.com".into()),
            name: Set("Owner".into()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        assert_eq!(
            eligible_owner(db.as_ref()).await.unwrap(),
            None,
            "nonadmin cannot adopt host credentials"
        );
        let role = roles::ActiveModel {
            name: Set("admin".into()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        user_roles::ActiveModel {
            user_id: Set(owner.id),
            role_id: Set(role.id),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        assert_eq!(eligible_owner(db.as_ref()).await.unwrap(), Some(owner.id));
        let mut deleted_owner: users::ActiveModel = owner.clone().into();
        deleted_owner.deleted_at = Set(Some(chrono::Utc::now()));
        deleted_owner.update(db.as_ref()).await.unwrap();
        assert_eq!(
            eligible_owner(db.as_ref()).await.unwrap(),
            None,
            "deleted sole owner cannot adopt credentials"
        );
        let mut restored_owner: users::ActiveModel = owner.clone().into();
        restored_owner.deleted_at = Set(None);
        restored_owner.update(db.as_ref()).await.unwrap();
        let second = users::ActiveModel {
            email: Set("second@example.com".into()),
            name: Set("Other".into()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        assert_eq!(
            eligible_owner(db.as_ref()).await.unwrap(),
            None,
            "multiple users skip import"
        );
        let mut deleted: users::ActiveModel = second.clone().into();
        deleted.deleted_at = Set(Some(chrono::Utc::now()));
        deleted.update(db.as_ref()).await.unwrap();
        assert_eq!(
            eligible_owner(db.as_ref()).await.unwrap(),
            None,
            "deleted owners still make identity ambiguous"
        );
        users::Entity::delete_by_id(second.id)
            .exec(db.as_ref())
            .await
            .unwrap();
        let encryption = EncryptionService::new("01234567890123456789012345678901").unwrap();
        let token = "synthetic-host-credential";
        let txn = db.begin().await.unwrap();
        let id = persist_import(
            &txn,
            &encryption,
            owner.id,
            Provider::Github,
            "example-owner".into(),
            token,
            "https://github.com".into(),
            "https://api.github.com".into(),
        )
        .await
        .unwrap()
        .unwrap();
        txn.commit().await.unwrap();
        let connection = git_provider_connections::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(connection.access_token.as_deref(), Some(token));
        assert_eq!(
            encryption
                .decrypt_string(connection.access_token.as_deref().unwrap())
                .unwrap(),
            token
        );
        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(!provider.auth_config.to_string().contains(token));
        assert!(
            matches!(serde_json::from_value::<AuthMethod>(provider.auth_config.clone()).unwrap(), AuthMethod::PersonalAccessToken { token } if token.is_empty())
        );
        assert!(already_imported(db.as_ref(), Provider::Github)
            .await
            .unwrap());
        let mut disconnected: git_provider_connections::ActiveModel = connection.clone().into();
        disconnected.is_active = Set(false);
        disconnected.update(db.as_ref()).await.unwrap();
        // A manually configured account is adopted without duplicating its connection.
        db.execute_unprepared("DELETE FROM host_git_imports WHERE provider = 'github'")
            .await
            .unwrap();
        let txn = db.begin().await.unwrap();
        assert_eq!(
            persist_import(
                &txn,
                &encryption,
                owner.id,
                Provider::Github,
                "example-owner".into(),
                token,
                "https://github.com".into(),
                "https://api.github.com".into()
            )
            .await
            .unwrap(),
            None
        );
        txn.commit().await.unwrap();
        assert!(already_imported(db.as_ref(), Provider::Github)
            .await
            .unwrap());
        assert!(
            !git_provider_connections::Entity::find_by_id(id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .is_active,
            "an existing disconnected account must not be reactivated"
        );
        // Deleting the connection/provider never deletes the import suppression marker.
        git_provider_connections::Entity::delete_by_id(id)
            .exec(db.as_ref())
            .await
            .unwrap();
        git_providers::Entity::delete_by_id(provider.id)
            .exec(db.as_ref())
            .await
            .unwrap();
        assert!(already_imported(db.as_ref(), Provider::Github)
            .await
            .unwrap());
        // A connection insert failure cannot leave an orphan provider or marker.
        db.execute_unprepared("ALTER TABLE git_provider_connections ADD CONSTRAINT reject_connection_test CHECK (account_name <> 'example-owner') NOT VALID").await.unwrap();
        let txn = db.begin().await.unwrap();
        let failure = persist_import(
            &txn,
            &encryption,
            owner.id,
            Provider::Gitlab,
            "example-owner".into(),
            token,
            "https://gitlab.com".into(),
            "https://gitlab.com/api/v4".into(),
        )
        .await;
        assert!(matches!(
            failure,
            Err(HostImportError::Database {
                operation: "insert connection",
                ..
            })
        ));
        txn.rollback().await.unwrap();
        assert!(!already_imported(db.as_ref(), Provider::Gitlab)
            .await
            .unwrap());
        assert!(git_providers::Entity::find()
            .all(db.as_ref())
            .await
            .unwrap()
            .is_empty());
        db.execute_unprepared(
            "ALTER TABLE git_provider_connections DROP CONSTRAINT reject_connection_test",
        )
        .await
        .unwrap();
        // Forced audit failure rolls back provider, connection and marker together.
        db.execute_unprepared("ALTER TABLE audit_logs ADD CONSTRAINT reject_import_test CHECK (operation_type <> 'HOST_GIT_PROVIDER_IMPORTED') NOT VALID").await.unwrap();
        let txn = db.begin().await.unwrap();
        let failure = persist_import(
            &txn,
            &encryption,
            owner.id,
            Provider::Gitlab,
            "example-owner".into(),
            token,
            "https://gitlab.com".into(),
            "https://gitlab.com/api/v4".into(),
        )
        .await;
        assert!(matches!(
            failure,
            Err(HostImportError::Database {
                operation: "audit import",
                ..
            })
        ));
        txn.rollback().await.unwrap();
        assert!(!already_imported(db.as_ref(), Provider::Gitlab)
            .await
            .unwrap());
        assert!(git_providers::Entity::find()
            .filter(git_providers::Column::ProviderType.eq("gitlab"))
            .all(db.as_ref())
            .await
            .unwrap()
            .is_empty());
    }
}
