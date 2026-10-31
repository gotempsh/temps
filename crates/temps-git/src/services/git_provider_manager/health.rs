// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connection health: probe a connection's token upstream and record the result.

use super::*;

const INITIAL_INSTALLATION_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl GitProviderManager {
    /// Validate connection by testing the access token
    pub async fn validate_connection(
        &self,
        connection_id: i32,
    ) -> Result<bool, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Decrypt access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Ok(false);
        };

        match provider_service.validate_token(&access_token).await {
            Ok(true) => Ok(true),
            Ok(false) => {
                // Token is invalid, mark connection as inactive
                self.deactivate_connection(connection_id).await?;
                Ok(false)
            }
            // A provider outage, rate limit, or missing operation permission
            // does not prove that the credential is invalid. Preserve the
            // typed error and keep the connection active.
            Err(error) => Err(error.into()),
        }
    }

    /// Run an immediate token probe against the upstream provider and persist
    /// `health_status` / `health_message` / `last_health_check_at` on the
    /// connection row. Intended to be called right after a connection is
    /// created (PAT or OAuth flows) so the UI reflects reality without waiting
    /// for the daily sweep.
    ///
    /// GitHub App installations are out of scope: they're probed via a
    /// different path (installation ID against the App API), which the
    /// `ConnectionHealthService` handles. For those, callers should invoke
    /// `connection_health_service.check_connection_health` instead.
    ///
    /// This method never errors on upstream failures — an unreachable or
    /// rejecting provider is simply recorded as `unhealthy`. It only returns
    /// `Err` for local issues (DB failure, missing connection, decrypt error).
    pub async fn probe_and_record_connection_health(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        use crate::services::connection_health::{HEALTH_STATUS_HEALTHY, HEALTH_STATUS_UNHEALTHY};

        let connection = self.get_connection(connection_id).await?;

        // GitHub App installations need installation-based probing, not token
        // validation. Skip here and let the scheduled sweep / explicit endpoint
        // handle them.
        if connection.installation_id.is_some() {
            return Ok(());
        }

        let access_token = match connection.access_token.as_deref() {
            Some(encrypted) => self.decrypt_string(encrypted).await?,
            None => {
                // No token to validate; mark unknown-but-unhealthy with reason.
                self.persist_health_status(
                    connection_id,
                    HEALTH_STATUS_UNHEALTHY,
                    Some("Connection has no access token".to_string()),
                )
                .await?;
                return Ok(());
            }
        };

        let provider_service = self.get_provider_service(connection.provider_id).await?;

        let (status, message) = match provider_service.validate_token(&access_token).await {
            Ok(true) => (HEALTH_STATUS_HEALTHY, None),
            Ok(false) => (
                HEALTH_STATUS_UNHEALTHY,
                Some("Access token rejected by provider (revoked or invalid)".to_string()),
            ),
            Err(GitProviderError::AuthenticationFailed(msg)) => (
                HEALTH_STATUS_UNHEALTHY,
                Some(format!("Authentication failed: {}", msg)),
            ),
            Err(e) => (
                HEALTH_STATUS_UNHEALTHY,
                Some(format!("Provider error: {}", e)),
            ),
        };

        self.persist_health_status(connection_id, status, message)
            .await?;
        Ok(())
    }

    /// Check a newly created GitHub App connection before repository syncing.
    /// The caller supplies installation verification because App credentials
    /// cannot use the PAT/OAuth token probe. Timeouts and upstream failures are
    /// persisted as unhealthy without undoing connection creation.
    pub(crate) async fn probe_and_record_initial_installation_health<F>(
        &self,
        connection: &git_provider_connections::Model,
        probe: F,
    ) -> Result<(), GitProviderManagerError>
    where
        F: std::future::Future<
            Output = Result<bool, crate::services::github::GithubAppServiceError>,
        >,
    {
        use crate::services::connection_health::{HEALTH_STATUS_HEALTHY, HEALTH_STATUS_UNHEALTHY};

        if connection.last_health_check_at.is_some() {
            return Ok(());
        }

        let (status, message) =
            match tokio::time::timeout(INITIAL_INSTALLATION_HEALTH_TIMEOUT, probe).await {
                Ok(Ok(true)) => (HEALTH_STATUS_HEALTHY, None),
                Ok(Ok(false)) => (
                    HEALTH_STATUS_UNHEALTHY,
                    Some(format!(
                        "GitHub App installation no longer exists or was suspended (connection {})",
                        connection.id
                    )),
                ),
                Ok(Err(error)) => (
                    HEALTH_STATUS_UNHEALTHY,
                    Some(format!(
                        "GitHub API error while verifying installation for connection {}: {}",
                        connection.id, error
                    )),
                ),
                Err(_) => (
                    HEALTH_STATUS_UNHEALTHY,
                    Some(format!(
                        "GitHub App health check for connection {} timed out after {} seconds",
                        connection.id,
                        INITIAL_INSTALLATION_HEALTH_TIMEOUT.as_secs()
                    )),
                ),
            };

        self.persist_health_status(connection.id, status, message)
            .await
    }

    /// Write the health result onto the connection row. Shared helper used by
    /// `probe_and_record_connection_health` so both the healthy and unhealthy
    /// branches go through the same update path.
    async fn persist_health_status(
        &self,
        connection_id: i32,
        status: &str,
        message: Option<String>,
    ) -> Result<(), GitProviderManagerError> {
        use crate::services::connection_health::HEALTH_STATUS_UNHEALTHY;

        let connection = self.get_connection(connection_id).await?;
        let now = chrono::Utc::now();
        let new_failures = if status == HEALTH_STATUS_UNHEALTHY {
            connection.consecutive_health_failures.saturating_add(1)
        } else {
            0
        };

        let mut active: git_provider_connections::ActiveModel = connection.into();
        active.health_status = Set(status.to_string());
        active.health_message = Set(message);
        active.last_health_check_at = Set(Some(now));
        active.consecutive_health_failures = Set(new_failures);
        active.updated_at = Set(now);
        active.update(self.db.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;

    fn initial_installation_health_fixture() -> (
        GitProviderManager,
        Arc<DatabaseConnection>,
        git_provider_connections::Model,
    ) {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let mut connection = connection_fixture(11, None);
        connection.installation_id = Some("456".to_string());
        connection.health_status = "unknown".to_string();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection.clone()], [connection.clone()]])
                .into_connection(),
        );
        let manager = initial_installation_health_manager(db.clone());
        (manager, db, connection)
    }

    fn initial_installation_health_manager(db: Arc<DatabaseConnection>) -> GitProviderManager {
        GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .unwrap(),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db.clone()),
        )
    }

    fn initial_installation_health_update(
        manager: GitProviderManager,
        db: Arc<DatabaseConnection>,
    ) -> String {
        drop(manager);
        let transactions = Arc::try_unwrap(db)
            .expect("health test should own its mock database")
            .into_transaction_log();
        let updates: Vec<_> = transactions
            .iter()
            .flat_map(|transaction| transaction.statements())
            .filter(|statement| statement.sql.starts_with("UPDATE"))
            .collect();
        assert_eq!(updates.len(), 1, "health should be recorded once");
        let update = updates[0].to_string();
        assert!(update.contains("\"last_health_check_at\" = '"));
        assert!(!update.contains("\"is_active\" ="));
        update
    }

    #[tokio::test]
    async fn initial_installation_health_records_success() {
        let (manager, db, connection) = initial_installation_health_fixture();
        manager
            .probe_and_record_initial_installation_health(&connection, async { Ok(true) })
            .await
            .unwrap();

        let update = initial_installation_health_update(manager, db);
        assert!(update.contains("\"health_status\" = 'healthy'"));
        assert!(update.contains("\"health_message\" = NULL"));
        assert!(update.contains("\"consecutive_health_failures\" = 0"));
    }

    #[tokio::test]
    async fn initial_installation_health_records_missing_installation() {
        let (manager, db, connection) = initial_installation_health_fixture();
        manager
            .probe_and_record_initial_installation_health(&connection, async { Ok(false) })
            .await
            .unwrap();

        let update = initial_installation_health_update(manager, db);
        assert!(update.contains("\"health_status\" = 'unhealthy'"));
        assert!(update.contains("installation no longer exists"));
        assert!(update.contains("\"consecutive_health_failures\" = 1"));
    }

    #[tokio::test]
    async fn initial_installation_health_records_upstream_error() {
        let (manager, db, connection) = initial_installation_health_fixture();
        manager
            .probe_and_record_initial_installation_health(&connection, async {
                Err(
                    crate::services::github::GithubAppServiceError::GithubApiError(
                        "temporary upstream failure".to_string(),
                    ),
                )
            })
            .await
            .unwrap();

        let update = initial_installation_health_update(manager, db);
        assert!(update.contains("\"health_status\" = 'unhealthy'"));
        assert!(update.contains("temporary upstream failure"));
        assert!(update.contains("\"consecutive_health_failures\" = 1"));
    }

    #[tokio::test]
    async fn initial_installation_health_times_out_and_records_failure() {
        let (manager, db, connection) = initial_installation_health_fixture();
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            manager
                .probe_and_record_initial_installation_health(&connection, std::future::pending()),
        )
        .await
        .expect("initial health probe must finish within its deadline")
        .unwrap();

        let update = initial_installation_health_update(manager, db);
        assert!(update.contains("\"health_status\" = 'unhealthy'"));
        assert!(update.contains("timed out after 10 seconds"));
        assert!(update.contains("\"consecutive_health_failures\" = 1"));
    }

    #[tokio::test]
    async fn initial_installation_health_skips_already_checked_connection() {
        let (manager, db, mut connection) = initial_installation_health_fixture();
        connection.last_health_check_at = Some(chrono::Utc::now());
        manager
            .probe_and_record_initial_installation_health(&connection, async {
                panic!("an already checked installation must not be probed again")
            })
            .await
            .unwrap();

        drop(manager);
        assert!(Arc::try_unwrap(db)
            .expect("health test should own its mock database")
            .into_transaction_log()
            .is_empty());
    }

    #[tokio::test]
    async fn initial_installation_health_propagates_recording_errors() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

        let connection = connection_fixture(11, None);
        let databases = [
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<git_provider_connections::Model>::new()]),
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("health read failed".to_string())]),
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection.clone()]])
                .append_query_errors([DbErr::Custom("health write failed".to_string())]),
        ];

        for (index, database) in databases.into_iter().enumerate() {
            let manager = initial_installation_health_manager(Arc::new(database.into_connection()));
            let error = manager
                .probe_and_record_initial_installation_health(&connection, async { Ok(true) })
                .await
                .unwrap_err();
            match (index, error) {
                (0, GitProviderManagerError::ConnectionNotFound(message)) => {
                    assert!(message.contains("11"));
                }
                (1 | 2, GitProviderManagerError::DatabaseError(DbErr::Custom(message))) => {
                    assert!(message.contains("health"));
                }
                (_, error) => panic!("unexpected recording error: {error}"),
            }
        }
    }
}
