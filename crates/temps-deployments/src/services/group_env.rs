// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Variables a service inherits from the project (group) it belongs to.
//! Which variables apply is decided once, in
//! [`temps_entities::project_group_env_vars::inherited_for_project`]; this
//! module only decrypts them for the deploy paths.

use std::collections::HashMap;

use sea_orm::{DatabaseConnection, DbErr};
use temps_core::EncryptionService;
use temps_entities::{environments, project_group_env_vars};
use thiserror::Error;

/// Why a service's inherited group variables could not be resolved. Every
/// variant names the project, the environment and (where known) the group
/// and variable key, so a failed deploy points at the exact row. Neither the
/// stored (encrypted) value nor a decrypted one is ever part of a message.
#[derive(Debug, Error)]
pub enum GroupEnvError {
    #[error(
        "Failed to load the variables project {project_id} inherits from its project group \
         (environment {environment_id}): {source}"
    )]
    Load {
        project_id: i32,
        environment_id: i32,
        #[source]
        source: DbErr,
    },

    #[error(
        "Failed to decrypt variable '{key}' of project group {group_id}, inherited by project \
         {project_id} (environment {environment_id}): {reason}"
    )]
    Decrypt {
        project_id: i32,
        environment_id: i32,
        group_id: i32,
        key: String,
        /// The decryption failure (wrong key, corrupt data), which describes
        /// the ciphertext's shape and never carries a value.
        reason: String,
    },
}

/// The decrypted variables `project_id` inherits in `environment`. Empty when
/// the service is in no project, so every deploy path can call it
/// unconditionally and a container never boots without them.
pub async fn inherited_group_env_vars(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    project_id: i32,
    environment: &environments::Model,
) -> Result<HashMap<String, String>, GroupEnvError> {
    let Some((group_id, selected)) =
        project_group_env_vars::inherited_for_project(db, project_id, environment)
            .await
            .map_err(|source| GroupEnvError::Load {
                project_id,
                environment_id: environment.id,
                source,
            })?
    else {
        return Ok(HashMap::new());
    };
    selected
        .into_iter()
        .map(|(key, variable)| {
            encryption
                .decrypt_string(&variable.value)
                .map(|value| (key.clone(), value))
                .map_err(|e| GroupEnvError::Decrypt {
                    project_id,
                    environment_id: environment.id,
                    group_id,
                    key,
                    reason: e.to_string(),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::project_group_members;

    use super::*;

    const PLAINTEXT: &str = "hunter2-plaintext-value";

    fn encryption() -> EncryptionService {
        EncryptionService::new(&"0".repeat(64)).expect("test encryption key")
    }

    fn environment(id: i32, slug: &str) -> environments::Model {
        environments::Model {
            id,
            name: slug.to_string(),
            slug: slug.to_string(),
            subdomain: format!("{slug}.example.com"),
            last_deployment: None,
            host: format!("{slug}.example.com"),
            upstreams: Default::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_id: 1,
            current_deployment_id: None,
            branch: Some("main".to_string()),
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            force_https: None,
            last_activity_at: None,
        }
    }

    fn membership(project_id: i32, group_id: i32) -> project_group_members::Model {
        project_group_members::Model {
            project_id,
            group_id,
            position: 0,
            inherits_repository: false,
            added_at: Utc::now(),
        }
    }

    fn variable(id: i32, group_id: i32, key: &str, stored: &str) -> project_group_env_vars::Model {
        project_group_env_vars::Model {
            id,
            group_id,
            key: key.to_string(),
            value: stored.to_string(),
            is_secret: true,
            environment_slug: None,
            include_in_preview: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_project_in_no_group_inherits_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<project_group_members::Model>::new()])
            .into_connection();
        let vars = inherited_group_env_vars(&db, &encryption(), 11, &environment(3, "production"))
            .await
            .expect("an ungrouped project is not an error");
        assert!(vars.is_empty());
    }

    #[tokio::test]
    async fn inherited_variables_are_decrypted() {
        let encryption = encryption();
        let stored = encryption.encrypt_string(PLAINTEXT).expect("encrypt");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(11, 5)]])
            .append_query_results([vec![variable(1, 5, "API_TOKEN", &stored)]])
            .into_connection();
        let vars = inherited_group_env_vars(&db, &encryption, 11, &environment(3, "production"))
            .await
            .expect("decrypts");
        assert_eq!(vars.get("API_TOKEN").map(String::as_str), Some(PLAINTEXT));
    }

    #[tokio::test]
    async fn a_database_failure_names_the_project_and_environment_and_keeps_its_source() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let error = inherited_group_env_vars(&db, &encryption(), 11, &environment(3, "production"))
            .await
            .expect_err("the query failed");

        match &error {
            GroupEnvError::Load {
                project_id,
                environment_id,
                source,
            } => {
                assert_eq!((*project_id, *environment_id), (11, 3));
                assert!(matches!(source, DbErr::Custom(m) if m == "connection reset"));
            }
            other => panic!("expected a load error, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("project 11"), "got {message}");
        assert!(message.contains("environment 3"), "got {message}");
        assert!(message.contains("connection reset"), "got {message}");
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn a_decryption_failure_names_the_variable_group_and_project_but_not_the_value() {
        // Encrypted under another key, so this deployment cannot read it.
        let other_key = EncryptionService::new(&"1".repeat(64)).expect("other key");
        let stored = other_key.encrypt_string(PLAINTEXT).expect("encrypt");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(11, 5)]])
            .append_query_results([vec![variable(9, 5, "API_TOKEN", &stored)]])
            .into_connection();
        let error = inherited_group_env_vars(&db, &encryption(), 11, &environment(3, "production"))
            .await
            .expect_err("wrong key");

        assert!(
            matches!(
                &error,
                GroupEnvError::Decrypt { project_id: 11, environment_id: 3, group_id: 5, key, .. }
                    if key == "API_TOKEN"
            ),
            "got {error:?}"
        );
        let message = error.to_string();
        for needle in [
            "'API_TOKEN'",
            "project group 5",
            "project 11",
            "environment 3",
        ] {
            assert!(message.contains(needle), "{needle} missing from {message}");
        }
        assert!(
            !message.contains(PLAINTEXT) && !message.contains(&stored),
            "the value must not be part of the message: {message}"
        );
        assert!(
            !format!("{error:?}").contains(PLAINTEXT) && !format!("{error:?}").contains(&stored),
            "the value must not be part of the debug output either"
        );
    }

    #[tokio::test]
    async fn stored_data_that_is_not_base64_does_not_echo_it() {
        let junk = "not-valid-base64-secret-looking-text!!";
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(11, 5)]])
            .append_query_results([vec![variable(9, 5, "DB_URL", junk)]])
            .into_connection();
        let error = inherited_group_env_vars(&db, &encryption(), 11, &environment(3, "production"))
            .await
            .expect_err("not decryptable");
        assert!(matches!(&error, GroupEnvError::Decrypt { key, .. } if key == "DB_URL"));
        assert!(!error.to_string().contains(junk), "got {error}");
    }
}
