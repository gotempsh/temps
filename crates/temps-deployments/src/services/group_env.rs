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

/// One decrypted variable a project inherits from its group, with the
/// sensitivity the group's admin gave it. Carrying `is_secret` beside the
/// value is what lets the planner keep a secret on the control plane while a
/// public one (`VITE_API_URL`) still reaches a worker build.
///
/// Deliberately not `Debug`-derived: the value must never reach a log line or
/// a panic message.
#[derive(Clone, PartialEq, Eq)]
pub struct InheritedGroupVariable {
    pub value: String,
    pub is_secret: bool,
}

impl std::fmt::Debug for InheritedGroupVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InheritedGroupVariable")
            .field("value", &"<redacted>")
            .field("is_secret", &self.is_secret)
            .finish()
    }
}

/// The decrypted variables `project_id` inherits in `environment`, by key.
/// Empty when the service is in no project, so every deploy path can call it
/// unconditionally and a container never boots without them.
pub async fn inherited_group_env_vars(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    project_id: i32,
    environment: &environments::Model,
) -> Result<HashMap<String, InheritedGroupVariable>, GroupEnvError> {
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
                .map(|value| {
                    (
                        key.clone(),
                        InheritedGroupVariable {
                            value,
                            is_secret: variable.is_secret,
                        },
                    )
                })
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

/// Lay `inherited` beneath the project's own variables: a key the project
/// already defines (secret or not, for this environment) keeps the project's
/// value, and every other key is added.
///
/// Returns the `(key, value)` pairs that were added **and** are not secret.
/// Those are the only inherited values a remote worker build may receive;
/// an inherited value the project overrode is never among them, so the
/// planner's public record can only ever hold the value the build carries.
pub fn merge_inherited_beneath(
    explicit_vars: &mut HashMap<String, String>,
    inherited: HashMap<String, InheritedGroupVariable>,
) -> Vec<(String, String)> {
    let mut public = Vec::new();
    for (key, variable) in inherited {
        if explicit_vars.contains_key(&key) {
            continue;
        }
        if !variable.is_secret {
            public.push((key.clone(), variable.value.clone()));
        }
        explicit_vars.insert(key, variable.value);
    }
    public
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
            shield_mode: None,
            shield_exempt_paths: Default::default(),
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
        let token = vars.get("API_TOKEN").expect("API_TOKEN is inherited");
        assert_eq!(token.value, PLAINTEXT);
        assert!(token.is_secret, "the stored sensitivity travels with it");
    }

    #[tokio::test]
    async fn sensitivity_is_carried_per_variable() {
        let encryption = encryption();
        let public_stored = encryption
            .encrypt_string("https://api.example.test")
            .expect("encrypt");
        let secret_stored = encryption.encrypt_string(PLAINTEXT).expect("encrypt");
        let mut public = variable(1, 5, "VITE_API_URL", &public_stored);
        public.is_secret = false;
        let secret = variable(2, 5, "API_TOKEN", &secret_stored);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(11, 5)]])
            .append_query_results([vec![public, secret]])
            .into_connection();
        let vars = inherited_group_env_vars(&db, &encryption, 11, &environment(3, "production"))
            .await
            .expect("decrypts");
        assert_eq!(
            vars.get("VITE_API_URL"),
            Some(&InheritedGroupVariable {
                value: "https://api.example.test".to_string(),
                is_secret: false,
            })
        );
        assert_eq!(
            vars.get("API_TOKEN"),
            Some(&InheritedGroupVariable {
                value: PLAINTEXT.to_string(),
                is_secret: true,
            })
        );
    }

    #[test]
    fn debug_output_never_contains_the_value() {
        let variable = InheritedGroupVariable {
            value: PLAINTEXT.to_string(),
            is_secret: true,
        };
        let rendered = format!("{variable:?}");
        assert!(!rendered.contains(PLAINTEXT), "got {rendered}");
        assert!(rendered.contains("is_secret: true"), "got {rendered}");
    }

    fn inherited(entries: &[(&str, &str, bool)]) -> HashMap<String, InheritedGroupVariable> {
        entries
            .iter()
            .map(|(key, value, is_secret)| {
                (
                    key.to_string(),
                    InheritedGroupVariable {
                        value: value.to_string(),
                        is_secret: *is_secret,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn merge_reports_only_added_non_secret_values_as_public() {
        let mut explicit = HashMap::new();
        let mut public = merge_inherited_beneath(
            &mut explicit,
            inherited(&[
                ("VITE_API_URL", "https://api.example.test", false),
                ("API_TOKEN", PLAINTEXT, true),
            ]),
        );
        public.sort();
        assert_eq!(
            public,
            vec![(
                "VITE_API_URL".to_string(),
                "https://api.example.test".to_string()
            )]
        );
        // Both are still part of the resolved variables.
        assert_eq!(explicit.len(), 2);
        assert_eq!(
            explicit.get("API_TOKEN").map(String::as_str),
            Some(PLAINTEXT)
        );
    }

    #[test]
    fn merge_keeps_the_project_value_and_does_not_report_the_overridden_one() {
        let mut explicit: HashMap<String, String> = [
            ("VITE_API_URL", "https://project.example.test"),
            ("SHARED_TOKEN", "project-secret"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let public = merge_inherited_beneath(
            &mut explicit,
            inherited(&[
                ("VITE_API_URL", "https://group.example.test", false),
                // A public group value under a name the project holds as a
                // secret must not become public either.
                ("SHARED_TOKEN", "group-public", false),
            ]),
        );
        assert!(public.is_empty(), "nothing was added");
        assert_eq!(
            explicit.get("VITE_API_URL").map(String::as_str),
            Some("https://project.example.test")
        );
        assert_eq!(
            explicit.get("SHARED_TOKEN").map(String::as_str),
            Some("project-secret")
        );
    }

    #[tokio::test]
    async fn another_environments_variable_is_not_inherited() {
        let encryption = encryption();
        let stored = encryption.encrypt_string("staging-only").expect("encrypt");
        let mut scoped = variable(1, 5, "VITE_API_URL", &stored);
        scoped.is_secret = false;
        scoped.environment_slug = Some("staging".to_string());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership(11, 5)]])
            .append_query_results([vec![scoped]])
            .into_connection();
        let vars = inherited_group_env_vars(&db, &encryption, 11, &environment(3, "production"))
            .await
            .expect("decrypts");
        assert!(vars.is_empty(), "got keys {:?}", vars.keys());
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
