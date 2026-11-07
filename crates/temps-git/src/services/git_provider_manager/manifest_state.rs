// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Single-use `state` tokens for the GitHub App manifest flow.
//!
//! The callback GitHub redirects to carries no session: the session cookie is
//! `SameSite=Strict`, so the browser does not send it on a cross-site redirect.
//! The state is therefore what ties the callback to the operator who started
//! the flow. It is issued by an authenticated endpoint, stored here, and
//! consumed (deleted) by the callback before it contacts GitHub, as the GitLab
//! callback does with `oauth_states`.

use super::*;

/// How long an operator has to finish creating the app on GitHub.
const MANIFEST_STATE_TTL_MINUTES: i64 = 15;

impl GitProviderManager {
    /// Issue a state for an operator who is starting the manifest flow.
    ///
    /// States nobody finished are deleted here, so the table stays small
    /// without a background job.
    pub async fn issue_github_manifest_state(
        &self,
        user_id: i32,
    ) -> Result<String, GitProviderManagerError> {
        use temps_entities::github_manifest_states;

        let now = chrono::Utc::now();
        github_manifest_states::Entity::delete_many()
            .filter(github_manifest_states::Column::ExpiresAt.lt(now))
            .exec(self.db.as_ref())
            .await?;

        let state = uuid::Uuid::new_v4().to_string();
        github_manifest_states::ActiveModel {
            state: Set(state.clone()),
            user_id: Set(user_id),
            expires_at: Set(now + chrono::Duration::minutes(MANIFEST_STATE_TTL_MINUTES)),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(state)
    }

    /// Consume a state, returning the operator who started the flow.
    ///
    /// The row is deleted as it is read, so a state works once even when two
    /// callbacks arrive together. A missing, unknown, expired or already used
    /// state is refused, and an expired one is still removed.
    pub async fn consume_github_manifest_state(
        &self,
        state: &str,
    ) -> Result<i32, GitProviderManagerError> {
        use temps_entities::github_manifest_states;

        let row = github_manifest_states::Entity::delete_many()
            .filter(github_manifest_states::Column::State.eq(state))
            .exec_with_returning(self.db.as_ref())
            .await?
            .into_iter()
            .next()
            .ok_or_else(invalid_manifest_state)?;
        if row.expires_at < chrono::Utc::now() {
            return Err(invalid_manifest_state());
        }
        Ok(row.user_id)
    }
}

fn invalid_manifest_state() -> GitProviderManagerError {
    GitProviderManagerError::OAuthStateInvalid(
        "The GitHub App setup link is invalid, expired or already used. Start the setup again from the console."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_entities::github_manifest_states;

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

    /// The SQL the manager ran, in order.
    fn statements(manager: GitProviderManager) -> Vec<(String, String)> {
        let db = manager.db.clone();
        drop(manager);
        Arc::try_unwrap(db)
            .expect("only the test owns the database")
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| {
                transaction
                    .statements()
                    .iter()
                    .map(|s| (s.sql.clone(), format!("{:?}", s.values)))
            })
            .collect()
    }

    fn state_row(
        state: &str,
        user_id: i32,
        minutes_from_now: i64,
    ) -> github_manifest_states::Model {
        github_manifest_states::Model {
            id: 1,
            state: state.to_string(),
            user_id,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(minutes_from_now),
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_pending_state_is_consumed_once_and_names_the_operator() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([[state_row(
                "pending-state",
                7,
                10,
            )]]),
        );

        let user_id = manager
            .consume_github_manifest_state("pending-state")
            .await
            .expect("a pending state is accepted");

        assert_eq!(user_id, 7);
        let statements = statements(manager);
        let sql: Vec<&String> = statements.iter().map(|(sql, _)| sql).collect();
        // Claimed by deleting the row as it is read, so a second callback
        // cannot also pass.
        assert_eq!(sql.len(), 1, "{sql:?}");
        assert!(
            sql[0].starts_with("DELETE FROM \"github_manifest_states\""),
            "{sql:?}"
        );
        assert!(sql[0].contains("RETURNING"), "{sql:?}");
    }

    #[tokio::test]
    async fn an_unknown_or_already_used_state_is_refused() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<github_manifest_states::Model>::new()]),
        );

        let error = manager
            .consume_github_manifest_state("never-issued")
            .await
            .expect_err("a state the server did not issue is refused");

        assert!(matches!(
            error,
            GitProviderManagerError::OAuthStateInvalid(_)
        ));
    }

    #[tokio::test]
    async fn an_expired_state_is_refused() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([[state_row(
                "stale-state",
                7,
                -1,
            )]]),
        );

        let error = manager
            .consume_github_manifest_state("stale-state")
            .await
            .expect_err("an expired state is refused");

        assert!(matches!(
            error,
            GitProviderManagerError::OAuthStateInvalid(_)
        ));
    }

    #[tokio::test]
    async fn issuing_a_state_stores_it_for_the_operator_and_clears_stale_ones() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                }])
                .append_query_results([[state_row("issued", 9, 15)]]),
        );

        let state = manager
            .issue_github_manifest_state(9)
            .await
            .expect("a state is issued");

        // A UUID v4: 36 characters, random.
        assert_eq!(state.len(), 36);
        let statements = statements(manager);
        let sql: Vec<&String> = statements.iter().map(|(sql, _)| sql).collect();
        assert_eq!(sql.len(), 2, "{sql:?}");
        assert!(
            sql[0].starts_with("DELETE FROM \"github_manifest_states\"")
                && sql[0].contains("\"expires_at\" <"),
            "stale states are cleared first: {sql:?}"
        );
        assert!(
            sql[1].starts_with("INSERT INTO \"github_manifest_states\""),
            "then the new state is stored: {sql:?}"
        );
        assert!(
            statements[1].1.contains(&state),
            "the state that is returned is the one that is stored"
        );
    }

    #[tokio::test]
    async fn two_issued_states_differ() {
        let manager = manager(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 0,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 0,
                    },
                ])
                .append_query_results([[state_row("a", 1, 15)], [state_row("b", 1, 15)]]),
        );

        let first = manager.issue_github_manifest_state(1).await.unwrap();
        let second = manager.issue_github_manifest_state(1).await.unwrap();

        assert_ne!(first, second);
    }
}
