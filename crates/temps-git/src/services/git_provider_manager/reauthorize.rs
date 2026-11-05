// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Replace an OAuth grant on its original connection, after authenticating the
//! initiating user again and verifying the upstream account identity.
use super::oauth::{invalid_oauth_state, OAuthCredentials};
use super::*;
use temps_entities::oauth_states;

impl GitProviderManager {
    pub async fn start_connection_reauthorization(
        &self,
        connection_id: i32,
        user_id: i32,
    ) -> Result<(String, i32), GitProviderManagerError> {
        let connection = self
            .reauthorizable_connection(connection_id, user_id)
            .await?;
        // Use the configured public origin, never a client-supplied callback URL.
        let external_url = self
            .config_service
            .get_setting("external_url")
            .await
            .map_err(|_| {
                GitProviderManagerError::InvalidConfiguration(
                    "Cannot read external_url; check the server settings before reauthorizing"
                        .to_string(),
                )
            })?
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                GitProviderManagerError::InvalidConfiguration(
                    "Set external_url in settings before reauthorizing a connection".to_string(),
                )
            })?;
        let host = format!("{}/api", external_url.trim_end_matches('/'));
        let (url, _) = self
            .start_oauth_flow_bound(
                connection.provider_id,
                user_id,
                Some(host),
                Some(connection_id),
            )
            .await?;
        Ok((url, connection.provider_id))
    }

    async fn reauthorizable_connection(
        &self,
        connection_id: i32,
        user_id: i32,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let connection = self.get_connection_for_user(connection_id, user_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;
        validate_reauthorization(&connection, &provider)?;
        let config = self.decrypt_sensitive_data(&provider.auth_config).await?;
        for field in ["client_id", "client_secret"] {
            if config[field].as_str().is_none_or(str::is_empty) {
                return Err(GitProviderManagerError::InvalidConfiguration(format!(
                    "Provider {} is missing {field}. Update its OAuth credentials before reauthorizing connection {connection_id}.", provider.id)));
            }
        }
        Ok(connection)
    }

    /// Public provider callbacks only relay codes to the console. They cannot
    /// consume reauthorization state or update credentials without RequireAuth.
    pub async fn reauthorization_redirect(
        &self,
        state: &str,
        provider_id: Option<i32>,
        code: Option<&str>,
    ) -> Result<Option<String>, GitProviderManagerError> {
        let Some(stored_state) = state.strip_prefix("reauthorize.") else {
            return Ok(None);
        };
        let row = oauth_states::Entity::find()
            .filter(oauth_states::Column::State.eq(stored_state))
            .filter(oauth_states::Column::ConnectionId.is_not_null())
            .one(self.db.as_ref())
            .await?;
        let Some(row) = row else {
            return Ok(Some(
                "/git-providers#reauthorize_error=invalid_state".to_string(),
            ));
        };
        if provider_id.is_some_and(|id| id != row.provider_id) {
            return Ok(Some(
                "/git-providers#reauthorize_error=invalid_state".to_string(),
            ));
        }
        // Relay even an expired state: the authenticated completion consumes
        // it and displays the failure in the console instead of a raw API page.
        // A fragment keeps the one-time code/state out of HTTP request and
        // referrer logs. The console removes it before calling the API.
        Ok(Some(format!(
            "/git-providers/{}#reauthorize_state={}&reauthorize_code={}",
            row.provider_id,
            urlencoding::encode(state),
            urlencoding::encode(code.unwrap_or_default())
        )))
    }

    async fn consume_reauthorization_state(
        &self,
        state: &str,
        user_id: i32,
    ) -> Result<oauth_states::Model, GitProviderManagerError> {
        let stored_state = state
            .strip_prefix("reauthorize.")
            .ok_or_else(invalid_oauth_state)?;
        let row = oauth_states::Entity::delete_many()
            .filter(oauth_states::Column::State.eq(stored_state))
            .filter(oauth_states::Column::UserId.eq(user_id))
            .filter(oauth_states::Column::ConnectionId.is_not_null())
            .exec_with_returning(self.db.as_ref())
            .await?
            .into_iter()
            .next()
            .ok_or_else(invalid_oauth_state)?;
        if row.expires_at < chrono::Utc::now() {
            return Err(invalid_oauth_state());
        }
        Ok(row)
    }

    pub async fn complete_connection_reauthorization(
        &self,
        state: &str,
        code: String,
        user_id: i32,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let state = self.consume_reauthorization_state(state, user_id).await?;
        let connection_id = state.connection_id.ok_or_else(invalid_oauth_state)?;
        let connection = self
            .reauthorizable_connection(connection_id, user_id)
            .await?;
        if connection.provider_id != state.provider_id {
            return Err(invalid_oauth_state());
        }
        if code.is_empty() {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Authorization for connection {connection_id} was cancelled or the provider returned no code. Try Reauthorize again.")));
        }
        let credentials = self
            .exchange_oauth_code(state.provider_id, code, state.callback_base_url)
            .await?;
        self.replace_oauth_grant(connection, credentials).await
    }

    pub(super) async fn replace_oauth_grant(
        &self,
        connection: git_provider_connections::Model,
        credentials: OAuthCredentials,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        verify_account(&connection, &credentials)?;
        let access = self.encrypt_string(&credentials.access_token).await?;
        let refresh = match credentials.refresh_token {
            Some(token) => Some(self.encrypt_string(&token).await?),
            None => None,
        };
        let now = chrono::Utc::now();
        let id = connection.id;
        let user_id = connection.user_id;
        let provider_id = connection.provider_id;
        let version = connection.updated_at;
        let generation = connection.credential_generation.wrapping_add(1);
        let mut update = git_provider_connections::ActiveModel {
            // Makes a refresh still in flight unable to overwrite this grant.
            credential_generation: Set(generation),
            access_token: Set(Some(access)),
            refresh_token: Set(refresh),
            token_expires_at: Set(credentials.expires_at),
            refresh_token_expires_at: Set(None),
            account_name: Set(credentials.account_name),
            is_expired: Set(false),
            is_active: Set(true),
            health_status: Set("healthy".to_string()),
            health_message: Set(None),
            consecutive_health_failures: Set(0),
            last_health_check_at: Set(Some(now)),
            updated_at: Set(now),
            ..Default::default()
        };
        // Retain connection-specific metadata such as webhook configuration.
        let mut metadata = connection.metadata.unwrap_or_else(|| json!({}));
        if let Some(object) = metadata.as_object_mut() {
            object.insert("id".to_string(), credentials.metadata["id"].clone());
        }
        update.metadata = Set(Some(metadata));
        // A concurrent edit/deletion/reauthorization must not be overwritten by
        // this older provider response. No database lock spans the HTTP exchange.
        let result = git_provider_connections::Entity::update_many()
            .set(update)
            .filter(git_provider_connections::Column::Id.eq(id))
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .filter(git_provider_connections::Column::UserId.eq(user_id))
            .filter(git_provider_connections::Column::UpdatedAt.eq(version))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected != 1 {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Connection {id} changed during authorization. Reload and try Reauthorize again."
            )));
        }
        self.get_connection(id).await
    }
}

fn validate_reauthorization(
    connection: &git_provider_connections::Model,
    provider: &git_providers::Model,
) -> Result<(), GitProviderManagerError> {
    if provider.auth_method != "oauth"
        || connection.installation_id.is_some()
        || !matches!(provider.provider_type.as_str(), "github" | "gitlab")
    {
        return Err(GitProviderManagerError::ReauthorizationUnsupported {
            connection_id: connection.id,
        });
    }
    if !provider.is_active {
        return Err(GitProviderManagerError::InvalidConfiguration(format!(
            "Provider {} is inactive. Activate it before reauthorizing connection {}.",
            provider.id, connection.id
        )));
    }
    Ok(())
}

fn verify_account(
    connection: &git_provider_connections::Model,
    credentials: &OAuthCredentials,
) -> Result<(), GitProviderManagerError> {
    let stored_id = connection
        .metadata
        .as_ref()
        .and_then(|m| m.get("id"))
        .filter(|id| !id.is_null());
    let matches = match stored_id {
        Some(id) => credentials.metadata.get("id") == Some(id),
        None => connection
            .account_name
            .eq_ignore_ascii_case(&credentials.account_name),
    };
    if !matches {
        return Err(GitProviderManagerError::ReauthorizationAccountMismatch {
            connection_id: connection.id,
            expected: connection.account_name.clone(),
            actual: credentials.account_name.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{
        ConnectionTrait, DatabaseBackend, IntoActiveModel, MockDatabase, MockExecResult,
    };

    fn manager(db: MockDatabase) -> GitProviderManager {
        let db = Arc::new(db.into_connection());
        GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .unwrap(),
            ),
            Arc::new(MockJobQueue),
            create_test_config_service(db),
            Arc::new(temps_core::NoopAuditLogger),
        )
    }

    fn transaction_log(manager: GitProviderManager) -> Vec<sea_orm::Transaction> {
        let db = manager.db.clone();
        drop(manager);
        Arc::try_unwrap(db)
            .expect("only test owns database")
            .into_transaction_log()
    }

    fn credentials() -> OAuthCredentials {
        OAuthCredentials {
            account_name: "example-account".into(),
            account_type: "User".into(),
            access_token: "replacement-access".into(),
            refresh_token: Some("replacement-refresh".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(2)),
            metadata: json!({"id": 123}),
        }
    }

    fn state_row() -> oauth_states::Model {
        oauth_states::Model {
            id: 1,
            state: "test-state".into(),
            user_id: 4,
            provider_id: 7,
            connection_id: Some(11),
            callback_base_url: Some("https://console.example.com/api".into()),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(15),
        }
    }

    #[tokio::test]
    async fn reauthorization_keeps_id_encrypts_tokens_and_clears_health() {
        let mut connection = connection_fixture(11, Some(4));
        connection.is_expired = true;
        connection.access_token = Some("previous-access-ciphertext".into());
        connection.refresh_token = Some("previous-refresh-ciphertext".into());
        connection.health_status = "unhealthy".into();
        connection.metadata = Some(json!({"id": 123, "webhook_id": "example-hook"}));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![connection.clone()]]);
        let manager = manager(db);
        let result = manager
            .replace_oauth_grant(connection, credentials())
            .await
            .unwrap();
        assert_eq!(result.id, 11);
        let encryption = manager.encryption_service.clone();
        let transactions = transaction_log(manager);
        let statement = &transactions[0].statements()[0];
        let sql = statement.to_string();
        let plaintexts: Vec<String> = statement
            .values
            .as_ref()
            .unwrap()
            .0
            .iter()
            .filter_map(|value| match value {
                sea_orm::Value::String(Some(value)) => encryption.decrypt_string(value).ok(),
                _ => None,
            })
            .collect();
        assert!(plaintexts.contains(&"replacement-access".to_string()));
        assert!(plaintexts.contains(&"replacement-refresh".to_string()));
        assert!(sql.contains("\"is_expired\" = FALSE"));
        assert!(sql.contains("\"credential_generation\" = 1"));
        assert!(sql.contains("\"health_message\" = NULL"));
        assert!(sql.contains("\"consecutive_health_failures\" = 0"));
        assert!(sql.contains("UPDATE"));
        assert!(!sql.contains("INSERT INTO"));
        assert!(sql.contains("health_status") && sql.contains("healthy"));
        assert!(sql.contains("consecutive_health_failures") && sql.contains("is_expired"));
        assert!(sql.contains("example-hook"));
        assert!(!sql.contains("replacement-access") && !sql.contains("replacement-refresh"));
        assert!(sql.contains("updated_at") && sql.contains("WHERE"));
    }

    #[tokio::test]
    async fn reauthorization_account_mismatch_leaves_row_untouched() {
        let mut connection = connection_fixture(11, Some(4));
        connection.metadata = Some(json!({"id": 999}));
        let mut grant = credentials();
        grant.account_name = "different-account".into();
        let manager = manager(MockDatabase::new(DatabaseBackend::Postgres));
        let error = manager
            .replace_oauth_grant(connection, grant)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            GitProviderManagerError::ReauthorizationAccountMismatch {
                connection_id: 11,
                ..
            }
        ));
        assert!(
            error.to_string().contains("example-account")
                && error.to_string().contains("different-account")
        );
        assert!(transaction_log(manager).is_empty());
    }

    #[test]
    fn reauthorization_stable_identity_allows_rename_but_not_same_name_different_id() {
        let mut connection = connection_fixture(11, Some(4));
        connection.metadata = Some(json!({"id": 123}));
        let mut grant = credentials();
        grant.account_name = "renamed-account".into();
        assert!(verify_account(&connection, &grant).is_ok());
        grant.account_name = connection.account_name.clone();
        grant.metadata = json!({"id": 456});
        assert!(verify_account(&connection, &grant).is_err());
        connection.metadata = None;
        assert!(verify_account(&connection, &grant).is_ok());
        grant.account_name = "another-account".into();
        assert!(verify_account(&connection, &grant).is_err());
    }

    #[test]
    fn reauthorization_refuses_non_oauth_and_inactive_providers() {
        let connection = connection_fixture(11, Some(4));
        let mut provider = github_provider_fixture();
        for method in ["pat", "github_pat", "github_app", "app", "basic", "ssh"] {
            provider.auth_method = method.into();
            assert!(matches!(
                validate_reauthorization(&connection, &provider),
                Err(GitProviderManagerError::ReauthorizationUnsupported { connection_id: 11 })
            ));
        }
        provider.auth_method = "oauth".into();
        assert!(validate_reauthorization(&connection, &provider).is_ok());
        provider.is_active = false;
        assert!(validate_reauthorization(&connection, &provider)
            .unwrap_err()
            .to_string()
            .contains("Activate"));
    }

    #[tokio::test]
    async fn reauthorization_missing_connection_is_not_found() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<git_provider_connections::Model>::new()]),
        );
        let error = manager
            .start_connection_reauthorization(11, 4)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            GitProviderManagerError::ConnectionNotFound(_)
        ));
        assert!(error.to_string().contains("11"));
    }

    #[tokio::test]
    async fn reauthorization_state_is_atomic_single_use_and_bound_to_user() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
                vec![state_row()],
                vec![],
                vec![],
            ]),
        );
        let row = manager
            .consume_reauthorization_state("reauthorize.test-state", 4)
            .await
            .unwrap();
        assert_eq!(row.connection_id, Some(11));
        assert!(manager
            .consume_reauthorization_state("reauthorize.test-state", 4)
            .await
            .is_err());
        assert!(manager
            .consume_reauthorization_state("reauthorize.forged-state", 4)
            .await
            .is_err());
        let sql = format!("{:?}", transaction_log(manager));
        assert!(sql.contains("DELETE FROM") && sql.contains("RETURNING"));
        assert!(sql.contains("user_id") && sql.contains("connection_id"));
    }

    #[tokio::test]
    async fn reauthorization_other_user_and_expired_state_are_refused() {
        let mut expired = state_row();
        expired.expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![], vec![expired]]),
        );
        assert!(manager
            .consume_reauthorization_state("reauthorize.test-state", 8)
            .await
            .is_err());
        assert!(manager
            .consume_reauthorization_state("reauthorize.test-state", 4)
            .await
            .is_err());
        let sql = format!("{:?}", transaction_log(manager));
        assert!(
            sql.contains("user_id") && sql.contains("Int(Some(8))"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn reauthorization_relay_binds_provider_and_encodes_fragment() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![state_row()], vec![state_row()]]),
        );
        assert_eq!(
            manager
                .reauthorization_redirect("reauthorize.test-state", Some(9), Some("code"))
                .await
                .unwrap()
                .as_deref(),
            Some("/git-providers#reauthorize_error=invalid_state")
        );
        let url = manager
            .reauthorization_redirect("reauthorize.test-state", Some(7), Some("code&injected=1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            url,
            "/git-providers/7#reauthorize_state=reauthorize.test-state&reauthorize_code=code%26injected%3D1"
        );
    }

    #[tokio::test]
    async fn reauthorization_refuses_stale_write() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]),
        );
        let error = manager
            .replace_oauth_grant(connection_fixture(11, Some(4)), credentials())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed during authorization"));
    }
    #[tokio::test]
    async fn reauthorization_refresh_rejects_stale_snapshot() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]),
        );
        let error = manager
            .update_connection_tokens(
                &connection_fixture(11, Some(4)),
                "stale-access".into(),
                Some("stale-refresh".into()),
                None,
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("Connection 11 changed during token refresh"));
        let sql = format!("{:?}", transaction_log(manager));
        let predicate = sql.split("WHERE").nth(1).unwrap();
        assert!(predicate.contains("credential_generation"));
        assert!(!predicate.contains("updated_at"));
        assert!(!sql.contains("stale-access") && !sql.contains("stale-refresh"));
    }

    #[tokio::test]
    async fn reauthorization_postgres_preserves_connection_and_claims_state_once() {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Skipping PostgreSQL reauthorization test: Docker unavailable");
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("Skipping PostgreSQL reauthorization test: Docker unavailable");
            return;
        }
        let database = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        database.connection().execute_unprepared("INSERT INTO users (id, name, email, created_at, updated_at) VALUES (7001, 'Example user', 'oauth@example.test', now(), now());
            INSERT INTO git_providers (id, name, provider_type, auth_method, auth_config, is_active, is_default, created_at, updated_at) VALUES (7001, 'Example OAuth', 'gitlab', 'oauth', '{}', true, false, now(), now());
            INSERT INTO git_provider_connections (id, provider_id, user_id, account_name, account_type, is_active, is_expired, health_status, health_message, consecutive_health_failures, metadata, created_at, updated_at) VALUES (7001, 7001, 7001, 'example-account', 'User', true, true, 'unhealthy', 'Expired', 3, '{\"id\":123,\"webhook_id\":\"example-hook\"}', now(), now());
            INSERT INTO oauth_states (state, user_id, provider_id, connection_id, expires_at, created_at) VALUES ('bound-state', 7001, 7001, 7001, now() + interval '15 minutes', now())").await.unwrap();
        let db = database.connection_arc();
        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .unwrap(),
            ),
            Arc::new(MockJobQueue),
            create_test_config_service(db),
            Arc::new(temps_core::NoopAuditLogger),
        );
        assert!(manager
            .consume_reauthorization_state("reauthorize.bound-state", 7002)
            .await
            .is_err());
        assert!(manager
            .consume_reauthorization_state("reauthorize.forged-state", 7001)
            .await
            .is_err());
        assert!(
            manager.consume_oauth_state("bound-state").await.is_err(),
            "reauthorization must never become a new connection"
        );
        let (first, second) = tokio::join!(
            manager.consume_reauthorization_state("reauthorize.bound-state", 7001),
            manager.consume_reauthorization_state("reauthorize.bound-state", 7001)
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert!(manager
            .consume_reauthorization_state("reauthorize.bound-state", 7001)
            .await
            .is_err());
        // Initially absent credentials exercise the NULL-safe credential predicate.
        let empty = manager.get_connection(7001).await.unwrap();
        manager
            .update_connection_tokens(
                &empty,
                "previous-access".into(),
                Some("previous-refresh".into()),
                None,
            )
            .await
            .unwrap();
        let original = manager.get_connection(7001).await.unwrap();
        assert_eq!(original.last_synced_at, None);

        // Health and failed-sync writes finish while the provider refresh is in flight.
        let last_sync = chrono::Utc::now() - chrono::Duration::hours(2);
        let sync_error_at = chrono::Utc::now() - chrono::Duration::hours(1);
        let mut status = original.clone().into_active_model();
        status.health_status = Set("unhealthy".into());
        status.health_message = Set(Some("Example health failure".into()));
        status.last_synced_at = Set(Some(last_sync));
        status.last_sync_error = Set(Some("Example repository sync failure".into()));
        status.last_sync_error_at = Set(Some(sync_error_at));
        let status = status.update(manager.db.as_ref()).await.unwrap();
        assert_ne!(status.updated_at, original.updated_at);
        manager
            .update_connection_tokens(
                &original,
                "refreshed-access".into(),
                Some("refreshed-refresh".into()),
                None,
            )
            .await
            .unwrap();
        let original = manager.get_connection(7001).await.unwrap();
        assert_eq!(original.health_status, status.health_status);
        assert_eq!(original.health_message, status.health_message);
        assert_eq!(original.last_synced_at, status.last_synced_at);
        assert_eq!(original.last_sync_error, status.last_sync_error);
        assert_eq!(original.last_sync_error_at, status.last_sync_error_at);
        assert!(original.last_synced_at < original.last_sync_error_at);
        assert_eq!(
            manager
                .decrypt_string(original.access_token.as_deref().unwrap())
                .await
                .unwrap(),
            "refreshed-access"
        );

        let mut wrong = credentials();
        wrong.metadata = json!({"id": 456});
        assert!(manager
            .replace_oauth_grant(original.clone(), wrong)
            .await
            .is_err());
        assert_eq!(manager.get_connection(7001).await.unwrap(), original);
        let connection = manager
            .replace_oauth_grant(original.clone(), credentials())
            .await
            .unwrap();
        // A refresh that began before consent completed must not replace the new grant.
        assert!(manager
            .update_connection_tokens(
                &original,
                "stale-access".into(),
                Some("stale-refresh".into()),
                None
            )
            .await
            .is_err());
        assert_eq!(manager.get_connection(7001).await.unwrap(), connection);
        assert_eq!(connection.id, 7001);
        assert_eq!(connection.provider_id, 7001);
        assert_eq!(connection.user_id, Some(7001));
        assert!(!connection.is_expired);
        assert_eq!(connection.health_status, "healthy");
        assert_eq!(connection.health_message, None);
        assert_eq!(connection.consecutive_health_failures, 0);
        assert!(connection.last_health_check_at.is_some());
        assert_eq!(connection.metadata.unwrap()["webhook_id"], "example-hook");
        assert_eq!(
            manager
                .decrypt_string(connection.access_token.as_deref().unwrap())
                .await
                .unwrap(),
            "replacement-access"
        );
        assert_eq!(
            manager
                .decrypt_string(connection.refresh_token.as_deref().unwrap())
                .await
                .unwrap(),
            "replacement-refresh"
        );
    }
    #[tokio::test]
    async fn reauthorization_start_binds_target_and_callback_in_persisted_state() {
        let mut provider = github_provider_fixture();
        provider.auth_method = "oauth".into();
        provider.auth_config =
            json!({"client_id": "example-client", "client_secret": "example-secret"});
        let settings = temps_entities::settings::Model {
            id: 1,
            data: json!({"external_url": "https://console.example.test"}),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![connection_fixture(11, Some(4))]])
                .append_query_results([vec![provider.clone()]])
                .append_query_results([vec![settings]])
                .append_query_results([vec![provider]])
                .append_query_results([vec![state_row()]]),
        );
        let (url, provider_id) = manager
            .start_connection_reauthorization(11, 4)
            .await
            .unwrap();
        assert_eq!(provider_id, 7);
        assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(url.contains("&state=reauthorize."));
        let transactions = transaction_log(manager);
        let sql = transactions.last().unwrap().statements()[0].to_string();
        assert!(sql.contains("INSERT INTO \"oauth_states\""));
        assert!(sql.contains("connection_id") && sql.contains("11"));
        assert!(sql.contains("https://console.example.test/api"));
    }

    #[tokio::test]
    async fn reauthorization_missing_oauth_credentials_are_actionable() {
        let mut provider = github_provider_fixture();
        provider.auth_method = "oauth".into();
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![connection_fixture(11, Some(4))]])
                .append_query_results([vec![provider]]),
        );
        let error = manager
            .start_connection_reauthorization(11, 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("client_id"));
        assert!(error.to_string().contains("Update its OAuth credentials"));
    }

    #[tokio::test]
    async fn reauthorization_cancellation_consumes_state_without_changing_connection() {
        let mut provider = github_provider_fixture();
        provider.auth_method = "oauth".into();
        provider.auth_config =
            json!({"client_id": "example-client", "client_secret": "example-secret"});
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![state_row()]])
                .append_query_results([vec![connection_fixture(11, Some(4))]])
                .append_query_results([vec![provider]]),
        );
        let error = manager
            .complete_connection_reauthorization("reauthorize.test-state", String::new(), 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        let sql = format!("{:?}", transaction_log(manager));
        assert!(sql.contains("DELETE FROM"));
        assert!(!sql.contains("UPDATE"));
    }
}
