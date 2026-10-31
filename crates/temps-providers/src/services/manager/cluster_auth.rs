// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster authentication secrets and auth-version upgrades.

use crate::services::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, EntityTrait, QuerySelect, Set, TransactionTrait};
use std::collections::HashMap;
use temps_entities::external_services;
use tracing::{debug, info, warn};

impl ExternalServiceManager {
    /// Create the per-app database `name` on the cluster's live
    /// primary if it doesn't already exist. Idempotent — uses
    /// `pg_database` lookup before issuing CREATE.
    ///
    /// The monitor selects the primary by persisted member identity; the
    /// credential destination is then rebuilt from stored topology and dialed
    /// through the pinned private-only PostgreSQL ladder. The control plane can
    /// reach worker-mapped ports because they bind to the worker's underlay IP.
    pub(super) async fn ensure_cluster_app_database(
        &self,
        service_id: i32,
        admin_user: &str,
        admin_password: &str,
        db_name: &str,
    ) -> Result<(), ExternalServiceError> {
        // Sanity-check the name matches what postgres allows for a
        // bare-quoted identifier — same rules the standalone path
        // applies. Strict to keep the CREATE DATABASE parameterless
        // safe (Postgres doesn't accept bind params for CREATE).
        if !db_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || db_name.is_empty()
            || db_name
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Cluster app DB name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                    db_name
                ),
            });
        }

        let (host, port) = match self.get_cluster_primary_address(service_id).await? {
            Some(hp) => hp,
            None => {
                return Err(ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot provision app database '{}' for cluster {}: \
                         no running primary",
                        db_name, service_id
                    ),
                });
            }
        };

        // SECURITY: use typed config setters so none of these values can inject
        // libpq connection-string parameters. The shared ladder resolves the
        // host once, pins the approved addresses, requires TLS on both TLS
        // rungs, and permits an unverified certificate or cleartext only for
        // those exact private addresses.
        let client = temps_query_postgres::connect_with_private_tls_ladder(
            &host,
            port,
            admin_user,
            admin_password,
            // Connect to the bootstrap DB to issue CREATE DATABASE; PostgreSQL
            // cannot create the database currently in use.
            "postgres",
        )
        .await
        .map_err(|error| ExternalServiceError::InternalError {
            reason: format!(
                "Failed to connect to cluster {} primary at {}:{} while provisioning database \
                 '{}': {}",
                service_id, host, port, db_name, error
            ),
        })?;

        // The owner, when the database exists: a sandbox's database must
        // never be handed to a deployment.
        let owner: Option<String> = client
            .query_opt(
                crate::sandbox_access::SandboxDatabasePlan::DATABASE_OWNER_QUERY,
                &[&db_name],
            )
            .await
            .and_then(|row| row.map(|row| row.try_get(0)).transpose())
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to check if database '{}' exists on cluster {}: {}",
                    db_name, service_id, e
                ),
            })?;
        crate::sandbox_access::check_deployment_database(db_name, owner.as_deref()).map_err(
            |conflict| ExternalServiceError::SandboxNamespaceConflict {
                service_id,
                conflict,
            },
        )?;

        if owner.is_some() {
            debug!(
                service_id,
                db_name, "App database already exists on cluster primary; skipping CREATE"
            );
            return Ok(());
        }

        // CREATE DATABASE doesn't accept bind params — the strict
        // identifier check above keeps this safe.
        let stmt = format!("CREATE DATABASE \"{}\"", db_name);
        client.execute(stmt.as_str(), &[]).await.map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to create database '{}' on cluster {}: {}",
                    db_name, service_id, e
                ),
            }
        })?;
        info!(
            service_id,
            db_name, "Created app database on cluster primary"
        );
        Ok(())
    }

    pub(super) async fn ensure_cluster_auth_secrets(
        &self,
        service_id: i32,
    ) -> Result<
        (
            HashMap<String, serde_json::Value>,
            crate::externalsvc::postgres_cluster::ClusterAuthSecrets,
            bool,
        ),
        ExternalServiceError,
    > {
        use crate::externalsvc::postgres_cluster::ClusterAuthSecrets;

        let txn = self.db.begin().await?;
        let service = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
        let encrypted =
            service
                .config
                .as_ref()
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!("Service {} has no config", service_id),
                })?;
        let config = self
            .encryption_service
            .decrypt_string(encrypted)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to decrypt cluster config: {}", e),
            })?;
        let mut parameters: HashMap<String, serde_json::Value> = serde_json::from_str(&config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to deserialize cluster config: {}", e),
            })?;
        let existing = ClusterAuthSecrets::from_parameters(&parameters).map_err(|e| {
            ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e.to_string(),
            }
        })?;
        if let Some(auth) = existing {
            txn.commit().await?;
            return Ok((parameters, auth, false));
        }

        let auth = ClusterAuthSecrets::generate();
        auth.insert_into(&mut parameters);
        let config_json = serde_json::to_string(&parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to serialize parameters of cluster service {}: {}",
                    service_id, e
                ),
            }
        })?;
        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to encrypt parameters of cluster service {}: {}",
                    service_id, e
                ),
            })?;
        let mut update: external_services::ActiveModel = service.into();
        update.config = Set(Some(encrypted_config));
        update.updated_at = Set(Utc::now());
        update.update(&txn).await?;
        txn.commit().await?;
        info!(
            service_id,
            "Generated SCRAM credentials for pg_auto_failover infrastructure roles"
        );
        Ok((parameters, auth, true))
    }

    /// Upgrade a running cluster created before SCRAM auth in place.
    ///
    /// Older clusters admit `autoctl_node` (monitor) and
    /// `pgautofailover_replicator` (data nodes) with `trust` from
    /// `0.0.0.0/0`. This runs `AuthUpgradeStep::ORDER` against every member
    /// over `docker exec` (local) or the agent (remote): passwords are set and
    /// distributed first, while the legacy rules still admit everyone, and
    /// only then are the rules switched to SCRAM. Any failure before the
    /// enforce steps leaves the cluster exactly as reachable as before, and
    /// the next control-plane start retries. Idempotent: an upgraded cluster
    /// converges with no change.
    ///
    /// Containers recreated later use the SCRAM entrypoints directly, and the
    /// legacy entrypoint of a container restarted after this upgrade leaves
    /// the rewritten rules alone (its patchers only add rules when no
    /// `0.0.0.0/0` rule for the role exists).
    ///
    /// A cluster that already converged at [`CLUSTER_INFRA_AUTH_VERSION`]
    /// only re-runs the data-node step, which switches its application rules
    /// to SCRAM once no login role stores an MD5 hash.
    pub async fn upgrade_cluster_auth(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        let parameters = self.get_service_parameters(service_id).await?;
        let version = cluster_auth_version(&parameters);
        let steps = cluster_auth_upgrade_steps(version);
        if steps.is_empty() {
            return Ok(());
        }
        // The staged rewrite is idempotent and every caller reuses the same
        // row-locked credentials. Do not hold a pooled database connection
        // across remote operations or nested database reads.
        self.apply_cluster_auth_upgrade(service_id, version, steps)
            .await?;
        self.mark_cluster_auth_completed(service_id).await
    }

    /// Bring a cluster's authentication up to date before a member is added.
    ///
    /// The new member is SCRAM-native, so the existing members must
    /// authenticate the infrastructure roles with SCRAM first; that failure is
    /// returned. The application-rule step of a cluster that is already past
    /// that point is not needed by the new member, and it cannot run while a
    /// data node is down — which is often why a member is being added — so
    /// its failure is logged and retried on the next start.
    pub(super) async fn prepare_cluster_auth_for_new_member(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        let Err(error) = self.upgrade_cluster_auth(service_id).await else {
            return Ok(());
        };
        let version = cluster_auth_version(&self.get_service_parameters(service_id).await?);
        if version < CLUSTER_INFRA_AUTH_VERSION {
            return Err(error);
        }
        warn!(
            service_id,
            auth_version = version,
            error = %error,
            "Adding a member without switching the cluster's application pg_hba rules \
             to SCRAM; the switch is retried on the next start"
        );
        Ok(())
    }

    async fn mark_cluster_auth_completed(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        self.record_cluster_auth_version(service_id, CLUSTER_AUTH_VERSION)
            .await
    }

    /// Raise a cluster's recorded authentication version to at least
    /// `reached`. Never lowers it.
    pub(super) async fn record_cluster_auth_version(
        &self,
        service_id: i32,
        reached: u64,
    ) -> Result<(), ExternalServiceError> {
        let txn = self.db.begin().await?;
        let service = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
        let encrypted = service.config.as_deref().ok_or_else(|| ExternalServiceError::InternalError {
            reason: format!("Cluster service {service_id} has no configuration while recording authentication convergence"),
        })?;
        let json = self
            .encryption_service
            .decrypt_string(encrypted)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to decrypt cluster config for service {service_id}: {e}"),
            })?;
        let mut parameters: HashMap<String, serde_json::Value> = serde_json::from_str(&json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to parse cluster config for service {service_id}: {e}"),
            })?;
        // Merge only the marker into the latest locked configuration, never a
        // pre-upgrade copy that can erase a concurrent settings update.
        let version = cluster_auth_version(&parameters).max(reached);
        parameters.insert(
            CLUSTER_AUTH_VERSION_KEY.to_string(),
            serde_json::json!(version),
        );
        let json = serde_json::to_string(&parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize cluster config for service {service_id}: {e}"),
            }
        })?;
        let encrypted = self.encryption_service.encrypt_string(&json).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt cluster config for service {service_id}: {e}"),
            }
        })?;
        external_services::ActiveModel {
            id: Set(service_id),
            config: Set(Some(encrypted)),
            ..Default::default()
        }
        .update(&txn)
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn apply_cluster_auth_upgrade(
        &self,
        service_id: i32,
        version: u64,
        steps: &[crate::externalsvc::postgres_cluster::AuthUpgradeStep],
    ) -> Result<(), ExternalServiceError> {
        use crate::externalsvc::postgres_cluster::PostgresClusterService;

        let (_, auth, _) = self.ensure_cluster_auth_secrets(service_id).await?;
        let members = self.get_service_members(service_id).await?;
        let targeted = |member: &ServiceMemberInfo| {
            steps
                .iter()
                .any(|step| is_role_monitor(&member.role) == step.targets_monitor())
        };
        if !members.iter().any(targeted) {
            return Ok(());
        }
        if let Some(not_running) = members
            .iter()
            .find(|m| targeted(m) && m.status != "running")
        {
            // Another caller can complete convergence and add a SCRAM-native
            // member after this caller's initial marker read.
            let latest = self.get_service_parameters(service_id).await?;
            if cluster_auth_version(&latest) >= CLUSTER_AUTH_VERSION {
                return Ok(());
            }
            let reason = if version >= CLUSTER_INFRA_AUTH_VERSION {
                format!(
                    "Cannot switch the application pg_hba rules of cluster service {} to \
                     SCRAM: member '{}' is '{}', not running. Every data node decides its \
                     own rules; the switch is retried on the next start.",
                    service_id, not_running.container_name, not_running.status
                )
            } else {
                format!(
                    "Cannot upgrade cluster service {} to SCRAM auth: member '{}' is '{}', not \
                     running. Every member must receive the new credentials before the \
                     trust rules are removed; the upgrade is retried on the next start.",
                    service_id, not_running.container_name, not_running.status
                )
            };
            return Err(ExternalServiceError::InternalError { reason });
        }

        for &step in steps {
            for member in members
                .iter()
                .filter(|m| is_role_monitor(&m.role) == step.targets_monitor())
            {
                let (exit_code, _stdout, stderr) = self
                    .exec_in_member_with_env(
                        member,
                        PostgresClusterService::auth_upgrade_command(step),
                        None,
                        auth.env(),
                    )
                    .await?;
                if exit_code != 0 {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "SCRAM auth upgrade step {:?} failed on member '{}' of cluster \
                             service {} (exit {}): {}",
                            step,
                            member.container_name,
                            service_id,
                            exit_code,
                            stderr.trim()
                        ),
                    });
                }
            }
        }
        info!(
            service_id,
            members = members.len(),
            from_auth_version = version,
            to_auth_version = CLUSTER_AUTH_VERSION,
            "Cluster authentication converged: infrastructure roles use SCRAM and every \
             data node decided its application pg_hba rules"
        );
        Ok(())
    }
}

#[cfg(test)]
#[cfg(feature = "docker-tests")]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use sea_orm::{ActiveModelTrait, Set};
    use std::sync::Arc;
    use temps_entities::external_services;

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn concurrent_cluster_auth_creation_reuses_one_credential_set() {
        let (original, test_db) = setup_test_manager_or_skip!();
        let mut options = sea_orm::ConnectOptions::new(test_db.database_url.clone());
        options.max_connections(1).min_connections(1);
        let db = Arc::new(sea_orm::Database::connect(options).await.unwrap());
        let manager = Arc::new(ExternalServiceManager::new(
            db.clone(),
            original.encryption_service.clone(),
            original.docker.cloned().unwrap(),
            Arc::new(temps_dns::DnsRegistry::new(db)),
        ));
        let config = manager
            .encryption_service
            .encrypt_string(r#"{"database":"kept"}"#)
            .unwrap();
        let service = external_services::ActiveModel {
            name: Set("concurrent-auth".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("cluster".to_string()),
            config: Set(Some(config)),
            ..Default::default()
        }
        .insert(manager.db.as_ref())
        .await
        .unwrap();
        let (a, b, c) = tokio::join!(
            manager.ensure_cluster_auth_secrets(service.id),
            manager.ensure_cluster_auth_secrets(service.id),
            manager.ensure_cluster_auth_secrets(service.id)
        );
        let results = [a.unwrap(), b.unwrap(), c.unwrap()];
        assert_eq!(results.iter().filter(|r| r.2).count(), 1);
        for result in &results {
            assert!(result.1 == results[0].1);
            assert_eq!(result.0.get("database"), Some(&serde_json::json!("kept")));
        }
        // Exercise the uncompleted path with only one pooled connection.
        manager.upgrade_cluster_auth(service.id).await.unwrap();
        // A completed upgrade can be checked concurrently without touching fake containers.
        let (first, second) = tokio::join!(
            manager.upgrade_cluster_auth(service.id),
            manager.upgrade_cluster_auth(service.id)
        );
        first.unwrap();
        second.unwrap();
        // A settings writer that read before generation must not erase passwords.
        let stale = manager
            .encryption_service
            .encrypt_string(r#"{"database":"changed"}"#)
            .unwrap();
        let mut update: external_services::ActiveModel = service.clone().into();
        update.config = Set(Some(stale));
        manager
            .persist_service_config(service.id, update)
            .await
            .unwrap();
        manager
            .mark_cluster_auth_completed(service.id)
            .await
            .unwrap();
        let persisted = manager.get_service_parameters(service.id).await.unwrap();
        assert_eq!(
            persisted.get("database"),
            Some(&serde_json::json!("changed"))
        );
        assert_eq!(
            persisted.get(CLUSTER_AUTH_VERSION_KEY),
            Some(&serde_json::json!(CLUSTER_AUTH_VERSION))
        );
        let old_null = manager.encryption_service.encrypt_string("null").unwrap();
        let legacy = external_services::ActiveModel {
            name: Set("legacy-null".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            config: Set(Some(old_null)),
            ..Default::default()
        }
        .insert(manager.db.as_ref())
        .await
        .unwrap();
        let config = manager
            .encryption_service
            .encrypt_string(r#"{"resources":{"cpu_cores":1}}"#)
            .unwrap();
        manager
            .persist_service_config(
                legacy.id,
                external_services::ActiveModel {
                    id: Set(legacy.id),
                    config: Set(Some(config)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            crate::externalsvc::postgres_cluster::ClusterAuthSecrets::from_parameters(&persisted)
                .unwrap()
                .unwrap()
                == results[0].1
        );
    }

    #[cfg(feature = "docker-tests")]
    async fn stored_auth_version(manager: &ExternalServiceManager, service_id: i32) -> u64 {
        cluster_auth_version(&manager.get_service_parameters(service_id).await.unwrap())
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn infra_converged_cluster_retries_app_rules_while_a_data_node_is_down() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let service_id = insert_cluster_at_auth_version(
            &manager,
            "auth-v1-node-down",
            Some(CLUSTER_INFRA_AUTH_VERSION),
            &[("monitor", "running"), ("replica", "stopped")],
        )
        .await;

        // A version-1 cluster is no longer treated as converged: it needs the
        // data-node step, which cannot reach the stopped replica.
        let error = manager.upgrade_cluster_auth(service_id).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("application pg_hba rules")
                && message.contains(&service_id.to_string())
                && message.contains("postgres-auth-v1-node-down-1")
                && message.contains("stopped"),
            "{message}"
        );
        assert_eq!(
            stored_auth_version(&manager, service_id).await,
            CLUSTER_INFRA_AUTH_VERSION,
            "a cluster whose data node never ran the step must not be marked converged"
        );

        // Replacing that node must still be possible: the new member only
        // needs SCRAM for the infrastructure roles, which version 1 proves.
        manager
            .prepare_cluster_auth_for_new_member(service_id)
            .await
            .unwrap();
        assert_eq!(
            stored_auth_version(&manager, service_id).await,
            CLUSTER_INFRA_AUTH_VERSION
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn legacy_cluster_with_a_member_down_still_blocks_adding_a_member() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let service_id = insert_cluster_at_auth_version(
            &manager,
            "auth-v0-node-down",
            None,
            &[("monitor", "running"), ("primary", "stopped")],
        )
        .await;

        let error = manager
            .prepare_cluster_auth_for_new_member(service_id)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("trust rules are removed"),
            "{error}"
        );
        assert_eq!(stored_auth_version(&manager, service_id).await, 0);
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn infra_converged_cluster_without_data_nodes_is_marked_current() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        // The data-node step has nothing to run on, and a stopped monitor is
        // not touched by it.
        let service_id = insert_cluster_at_auth_version(
            &manager,
            "auth-v1-monitor-only",
            Some(CLUSTER_INFRA_AUTH_VERSION),
            &[("monitor", "stopped")],
        )
        .await;

        manager.upgrade_cluster_auth(service_id).await.unwrap();
        assert_eq!(
            stored_auth_version(&manager, service_id).await,
            CLUSTER_AUTH_VERSION
        );
        // Converged clusters return without reading members again.
        manager.upgrade_cluster_auth(service_id).await.unwrap();
    }

    /// Regression for #1352: a cluster built by `initialize_cluster` records
    /// that its infrastructure roles are SCRAM-native, so adding a member
    /// does not re-run the legacy NodePrepare step against existing members.
    /// Recording that never lowers a version a later upgrade reached.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn natively_created_cluster_records_infrastructure_auth_without_lowering() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let service_id = insert_cluster_at_auth_version(
            &manager,
            "auth-native-new",
            None,
            &[("monitor", "running"), ("primary", "running")],
        )
        .await;
        assert_eq!(
            cluster_auth_upgrade_steps(stored_auth_version(&manager, service_id).await),
            &crate::externalsvc::postgres_cluster::AuthUpgradeStep::ORDER
        );

        manager
            .record_cluster_auth_version(service_id, CLUSTER_INFRA_AUTH_VERSION)
            .await
            .unwrap();
        assert_eq!(
            stored_auth_version(&manager, service_id).await,
            CLUSTER_INFRA_AUTH_VERSION
        );
        assert_eq!(
            cluster_auth_upgrade_steps(CLUSTER_INFRA_AUTH_VERSION),
            &[crate::externalsvc::postgres_cluster::AuthUpgradeStep::NodeEnforce],
            "a native cluster must not re-run NodePrepare when a member is added"
        );

        manager
            .mark_cluster_auth_completed(service_id)
            .await
            .unwrap();
        manager
            .record_cluster_auth_version(service_id, CLUSTER_INFRA_AUTH_VERSION)
            .await
            .unwrap();
        assert_eq!(
            stored_auth_version(&manager, service_id).await,
            CLUSTER_AUTH_VERSION
        );
    }
}
