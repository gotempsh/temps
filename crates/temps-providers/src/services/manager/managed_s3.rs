// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed S3 backends, bucket scopes and S3 migration.

use crate::externalsvc::{
    garage::GarageService, managed_instance_name, rustfs::RustfsService, ManagedS3Backend,
    ManagedS3BackendKind, ManagedS3BackendSelection, ManagedS3Error, MigrationSource,
    ServiceConfig, ServiceType,
};
use crate::services::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{BTreeSet, HashMap};
use temps_entities::{external_services, project_services, projects};
use tracing::{debug, info};

impl ExternalServiceManager {
    /// The managed-S3 engine handle for a service, or `None` when the service
    /// is not managed S3 storage (or is a legacy MinIO service, which has no
    /// per-environment credentials to manage).
    fn managed_s3_backend(
        &self,
        name: &str,
        service_type: ServiceType,
        parameters: &serde_json::Value,
    ) -> Result<Option<Box<dyn ManagedS3Backend>>, ExternalServiceError> {
        if !matches!(
            service_type,
            ServiceType::S3 | ServiceType::Blob | ServiceType::Rustfs
        ) {
            return Ok(None);
        }
        let selection = ManagedS3BackendSelection::from_parameters(parameters).map_err(|e| {
            ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason: e.to_string(),
            }
        })?;
        let docker = self.require_docker()?;
        match selection.backend {
            ManagedS3BackendKind::Minio => Ok(None),
            ManagedS3BackendKind::Rustfs => {
                let instance_name = if service_type == ServiceType::Blob {
                    managed_instance_name(name, service_type)
                } else {
                    name.to_string()
                };
                Ok(Some(Box::new(RustfsService::new(
                    instance_name,
                    docker,
                    self.encryption_service.clone(),
                ))))
            }
            ManagedS3BackendKind::Garage => Ok(Some(Box::new(GarageService::new(
                name.to_string(),
                docker,
                self.encryption_service.clone(),
                service_type,
            )))),
        }
    }

    /// Bucket-name scopes (see `managed_s3::scope_slug`) that links of
    /// `service_id` held by projects other than `excluding_project_id` map
    /// to, each with the `(project_scope, environment_scope)` pairs behind it.
    ///
    /// `scope_slug` joins the two parts with `-`, so different pairs can
    /// produce one slug (`acme-web` + `prod` and `acme` + `web-prod`). One
    /// slug means one bucket and one derived key, so callers use this to
    /// refuse a second tenant and to keep a revoke from reaching the first.
    /// Three queries regardless of the number of links.
    async fn other_projects_s3_scopes(
        &self,
        service_id: i32,
        excluding_project_id: i32,
    ) -> Result<HashMap<String, BTreeSet<(String, String)>>, ExternalServiceError> {
        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .filter(project_services::Column::ProjectId.ne(excluding_project_id))
            .all(self.db.as_ref())
            .await?;
        if links.is_empty() {
            return Ok(HashMap::new());
        }
        let project_ids: Vec<i32> = links.iter().map(|link| link.project_id).collect();
        let project_slugs: HashMap<i32, String> = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids.clone()))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|project| (project.id, project.slug))
            .collect();
        let mut environment_slugs: HashMap<i32, Vec<String>> = HashMap::new();
        for environment in temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.is_in(project_ids))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?
        {
            environment_slugs
                .entry(environment.project_id)
                .or_default()
                .push(environment.slug);
        }

        let mut scopes: HashMap<String, BTreeSet<(String, String)>> = HashMap::new();
        for link in &links {
            let Some(project_slug) = project_slugs.get(&link.project_id) else {
                continue;
            };
            let provisioning = DatabaseProvisioningConfig::from_link(link)?;
            let empty = Vec::new();
            let environments = environment_slugs.get(&link.project_id).unwrap_or(&empty);
            for environment_slug in environments {
                let (project_scope, environment_scope) = provisioning.runtime_scope(
                    project_slug,
                    environment_slug,
                    service_id,
                    link.project_id,
                )?;
                scopes
                    .entry(crate::externalsvc::managed_s3::scope_slug(
                        &project_scope,
                        &environment_scope,
                    ))
                    .or_default()
                    .insert((project_scope, environment_scope));
            }
        }
        Ok(scopes)
    }

    /// Refuse to provision a managed S3 scope whose buckets another project's
    /// link already owns under a different `(project, environment)` pair.
    ///
    /// Compared on the final physical bucket names, not on the scope slugs:
    /// with declared buckets `uploads` and `media-uploads`, the scopes
    /// `shop-prod` and `shop-prod-media` differ yet both name
    /// `shop-prod-media-uploads`. Linking one custom scope from several
    /// projects is deliberate sharing (the pair is identical) and passes.
    pub(super) async fn ensure_s3_scope_unambiguous(
        &self,
        service_id: i32,
        service_type: &str,
        parameters: &HashMap<String, serde_json::Value>,
        project_id: i32,
        project_scope: &str,
        environment_scope: &str,
    ) -> Result<(), ExternalServiceError> {
        if !matches!(service_type, "s3" | "blob" | "rustfs") {
            return Ok(());
        }
        let declared = ManagedS3BackendSelection::from_parameters(
            &serde_json::to_value(parameters).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to inspect managed S3 bucket parameters: {}", e),
            })?,
        )
        .map_err(|e| ExternalServiceError::ParameterValidationFailed {
            service_id,
            reason: e.to_string(),
        })?
        .buckets;
        let physical_names = |slug: &str| -> BTreeSet<String> {
            crate::externalsvc::managed_s3::plan_buckets(slug, &declared)
                .into_iter()
                .map(|plan| plan.physical)
                .collect()
        };

        let own_slug = crate::externalsvc::managed_s3::scope_slug(project_scope, environment_scope);
        let own_buckets = physical_names(&own_slug);
        let own = (project_scope.to_string(), environment_scope.to_string());
        let others = self
            .other_projects_s3_scopes(service_id, project_id)
            .await?;
        for (other_slug, pairs) in &others {
            let Some(conflicting) = pairs.iter().find(|pair| **pair != own) else {
                continue;
            };
            if let Some(bucket) = physical_names(other_slug).intersection(&own_buckets).next() {
                return Err(ExternalServiceError::InvalidDatabaseProvisioning {
                    service_id,
                    project_id,
                    reason: format!(
                        "bucket '{bucket}' for project '{project_scope}' environment '{environment_scope}' is already used by project '{}' environment '{}' on this service; \
                         rename a project, an environment or a declared bucket, or link a different service",
                        conflicting.0, conflicting.1
                    ),
                });
            }
        }
        Ok(())
    }

    /// Revoke the credentials a project link provisioned for its
    /// environments, locally or through the agent that owns a remote service.
    /// Scopes still shared by another link are kept.
    pub(super) async fn revoke_managed_s3_credentials(
        &self,
        service: &external_services::Model,
        link: &project_services::Model,
    ) -> Result<(), ExternalServiceError> {
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        if !matches!(
            service_type,
            ServiceType::S3 | ServiceType::Blob | ServiceType::Rustfs
        ) {
            return Ok(());
        }
        let config = self.get_service_config(service.id).await?;
        // A remote service's engine is only reachable from its worker, so the
        // owning agent performs the revocation there.
        let remote_node = match service_execution_route(service.node_id) {
            ServiceExecutionRoute::Remote(node_id) => Some(node_id),
            ServiceExecutionRoute::Local => None,
        };
        let backend = if remote_node.is_some() {
            let selection = ManagedS3BackendSelection::from_parameters(&config.parameters)
                .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                    service_id: service.id,
                    reason: e.to_string(),
                })?;
            if selection.backend == ManagedS3BackendKind::Minio {
                return Ok(());
            }
            None
        } else {
            match self.managed_s3_backend(&service.name, service_type, &config.parameters)? {
                Some(backend) => Some(backend),
                None => return Ok(()),
            }
        };

        let provisioning = DatabaseProvisioningConfig::from_link(link)?;
        let project = projects::Entity::find_by_id(link.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound {
                id: link.project_id,
            })?;
        let environments = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(link.project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        let mut scopes = BTreeSet::new();
        for environment in &environments {
            scopes.insert(provisioning.runtime_scope(
                &project.slug,
                &environment.slug,
                service.id,
                link.project_id,
            )?);
        }

        // A scope can be held by several projects: a custom scope is shared
        // on purpose, and two different (project, environment) pairs can
        // share a bucket slug. Its credentials stay until the last holder is
        // unlinked.
        let others = self
            .other_projects_s3_scopes(service.id, link.project_id)
            .await?;
        let scopes: BTreeSet<(String, String)> = scopes
            .into_iter()
            .filter(|(project_scope, environment_scope)| {
                let slug =
                    crate::externalsvc::managed_s3::scope_slug(project_scope, environment_scope);
                let still_shared = others.contains_key(&slug);
                if still_shared {
                    debug!(
                        service_id = service.id,
                        project_id = link.project_id,
                        scope = %slug,
                        "Keeping managed S3 credentials: the scope is linked by another project"
                    );
                }
                !still_shared
            })
            .collect();

        for (project_scope, environment_scope) in scopes {
            match (&backend, remote_node) {
                (Some(backend), _) => backend
                    .revoke_access_key(&config, &project_scope, &environment_scope)
                    .await
                    .map_err(|e| Self::managed_s3_error(service.id, e))?,
                (None, Some(node_id)) => {
                    self.get_remote_client(node_id)
                        .await?
                        .revoke_s3_access(crate::remote_service_client::RemoteRuntimeEnvRequest {
                            service_config: config.clone(),
                            project_slug: project_scope.clone(),
                            environment_slug: environment_scope.clone(),
                        })
                        .await?;
                }
                (None, None) => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "No storage engine available to revoke credentials of service {}",
                            service.id
                        ),
                    });
                }
            }
            info!(
                service_id = service.id,
                project_id = link.project_id,
                project_scope = %project_scope,
                environment_scope = %environment_scope,
                "Revoked managed S3 credentials"
            );
        }
        Ok(())
    }

    fn managed_s3_error(service_id: i32, error: ManagedS3Error) -> ExternalServiceError {
        match error {
            ManagedS3Error::UnsupportedBackend { .. }
            | ManagedS3Error::BackendNotAString { .. }
            | ManagedS3Error::DeprecatedBackend
            | ManagedS3Error::BucketsNotAList { .. }
            | ManagedS3Error::InvalidBucketName { .. }
            | ManagedS3Error::TooManyBuckets { .. }
            | ManagedS3Error::InvalidConfig { .. }
            | ManagedS3Error::Unsupported { .. } => {
                ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: error.to_string(),
                }
            }
            ManagedS3Error::Admin { .. }
            | ManagedS3Error::NotReady { .. }
            | ManagedS3Error::Docker { .. }
            | ManagedS3Error::Mirror { .. } => ExternalServiceError::InternalError {
                reason: format!(
                    "Managed S3 operation failed for service {}: {}",
                    service_id, error
                ),
            },
        }
    }

    /// Copy every bucket of `source_service_id` into `target_service_id` with
    /// plain S3 semantics, so data can move between backends (RustFS ↔
    /// Garage). Existing objects in the target are left in place; the source
    /// is never modified.
    pub async fn migrate_s3_service(
        &self,
        target_service_id: i32,
        source_service_id: i32,
    ) -> Result<S3MigrationReport, ExternalServiceError> {
        if target_service_id == source_service_id {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: target_service_id,
                reason: "cannot migrate a service into itself".to_string(),
            });
        }
        let target = self.get_service(target_service_id).await?;
        let source = self.get_service(source_service_id).await?;
        for service in [&target, &source] {
            if service.node_id.is_some() {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: service.id,
                    reason: format!(
                        "service {} ('{}') is hosted on a remote node; S3 migration runs on the control plane against local services only",
                        service.id, service.name
                    ),
                });
            }
        }

        let target_config = self.get_service_config(target.id).await?;
        let source_config = self.get_service_config(source.id).await?;
        let backend_of = |service: &external_services::Model,
                          config: &ServiceConfig|
         -> Result<Box<dyn ManagedS3Backend>, ExternalServiceError> {
            self.managed_s3_backend(&service.name, config.service_type, &config.parameters)?
                .ok_or_else(|| ExternalServiceError::ParameterValidationFailed {
                    service_id: service.id,
                    reason: format!(
                        "service {} ('{}') is not a managed S3 service with a migratable backend (legacy MinIO services cannot be migrated)",
                        service.id, service.name
                    ),
                })
        };
        let target_backend = backend_of(&target, &target_config)?;
        let source_backend = backend_of(&source, &source_config)?;

        let (admin_access_key, admin_secret_key) = source_backend
            .admin_credentials(&source_config)
            .map_err(|e| Self::managed_s3_error(source.id, e))?;
        let migration_source = MigrationSource {
            service: source_backend.service_name(),
            backend: source_backend.kind(),
            endpoint: source_backend
                .endpoint(&source_config)
                .map_err(|e| Self::managed_s3_error(source.id, e))?,
            admin_access_key,
            admin_secret_key,
        };

        info!(
            source_service_id = source.id,
            target_service_id = target.id,
            source_backend = %source_backend.kind().as_str(),
            target_backend = %target_backend.kind().as_str(),
            "Migrating S3 data between managed services"
        );
        let report = target_backend
            .migrate_from(&target_config, &migration_source)
            .await
            .map_err(|e| Self::managed_s3_error(target.id, e))?;

        Ok(S3MigrationReport {
            source_service_id: source.id,
            target_service_id: target.id,
            source_backend: source_backend.kind().as_str().to_string(),
            target_backend: target_backend.kind().as_str().to_string(),
            buckets: report.buckets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Arc;
    use temps_entities::{project_services, projects};

    fn s3_link(project_id: i32, mode: &str) -> project_services::Model {
        let now = Utc::now();
        project_services::Model {
            id: project_id,
            project_id,
            service_id: 41,
            database_provisioning_mode: mode.to_string(),
            custom_database_name: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn scope_project(id: i32, slug: &str) -> projects::Model {
        let mut project = environment_preview_project(id);
        project.slug = slug.to_string();
        project
    }

    fn scope_environment(
        id: i32,
        project_id: i32,
        slug: &str,
    ) -> temps_entities::environments::Model {
        let now = Utc::now();
        temps_entities::environments::Model {
            id,
            name: slug.to_string(),
            slug: slug.to_string(),
            subdomain: format!("{slug}-{project_id}"),
            last_deployment: None,
            host: String::new(),
            upstreams: Default::default(),
            created_at: now,
            updated_at: now,
            project_id,
            current_deployment_id: None,
            branch: None,
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
            ingress_mode: Default::default(),
            workload_mode: Default::default(),
        }
    }

    /// Project `acme` + environment `web-prod` is already linked; the manager
    /// is asked about project `acme-web` + `prod`.
    fn manager_with_ambiguous_scope_holder() -> ExternalServiceManager {
        mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![s3_link(7, "project_environment")]])
                .append_query_results([vec![scope_project(7, "acme")]])
                .append_query_results([vec![scope_environment(70, 7, "web-prod")]])
                .into_connection(),
        ))
    }

    #[tokio::test]
    async fn a_second_project_with_the_same_bucket_scope_is_refused() {
        let manager = manager_with_ambiguous_scope_holder();

        let error = manager
            .ensure_s3_scope_unambiguous(41, "s3", &HashMap::new(), 8, "acme-web", "prod")
            .await
            .expect_err("two different pairs must not share one bucket scope");

        match error {
            ExternalServiceError::InvalidDatabaseProvisioning {
                service_id: 41,
                project_id: 8,
                reason,
            } => {
                assert!(reason.contains("acme-web-prod"), "{reason}");
                assert!(reason.contains("project 'acme'"), "{reason}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// Declared buckets `uploads` and `media-uploads` make the project-wide
    /// scopes `shop-prod` and `shop-prod-media` both name
    /// `shop-prod-media-uploads`, though the scope slugs differ.
    #[tokio::test]
    async fn scopes_with_different_slugs_but_one_physical_bucket_are_refused() {
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![s3_link(7, "project")]])
                .append_query_results([vec![scope_project(7, "shop-prod-media")]])
                .append_query_results([vec![scope_environment(70, 7, "production")]])
                .into_connection(),
        ));
        let parameters = HashMap::from([(
            "buckets".to_string(),
            serde_json::json!(["uploads", "media-uploads"]),
        )]);

        let error = manager
            .ensure_s3_scope_unambiguous(41, "s3", &parameters, 8, "shop-prod", "")
            .await
            .expect_err("the second project would share a physical bucket");

        match error {
            ExternalServiceError::InvalidDatabaseProvisioning { reason, .. } => {
                assert!(reason.contains("shop-prod-media-uploads"), "{reason}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn scopes_whose_physical_buckets_differ_may_coexist() {
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![s3_link(7, "project")]])
                .append_query_results([vec![scope_project(7, "shop-prod-media")]])
                .append_query_results([vec![scope_environment(70, 7, "production")]])
                .into_connection(),
        ));
        let parameters = HashMap::from([("buckets".to_string(), serde_json::json!(["uploads"]))]);

        manager
            .ensure_s3_scope_unambiguous(41, "s3", &parameters, 8, "shop-prod", "")
            .await
            .expect("shop-prod-uploads and shop-prod-media-uploads are different buckets");
    }

    #[tokio::test]
    async fn the_same_scope_pair_held_by_another_project_is_deliberate_sharing() {
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![s3_link(7, "project_environment")]])
                .append_query_results([vec![scope_project(7, "acme")]])
                .append_query_results([vec![scope_environment(70, 7, "prod")]])
                .into_connection(),
        ));

        manager
            .ensure_s3_scope_unambiguous(41, "s3", &HashMap::new(), 8, "acme", "prod")
            .await
            .expect("an identical pair is not ambiguous");
    }

    #[tokio::test]
    async fn the_scope_guard_ignores_non_s3_services() {
        let manager = mock_service_manager(vec![]);

        manager
            .ensure_s3_scope_unambiguous(41, "postgres", &HashMap::new(), 8, "acme-web", "prod")
            .await
            .expect("only managed S3 scopes are checked");
    }

    #[tokio::test]
    async fn other_projects_scopes_are_empty_when_nothing_else_is_linked() {
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_services::Model>::new()])
                .into_connection(),
        ));

        let scopes = manager
            .other_projects_s3_scopes(41, 8)
            .await
            .expect("scopes");

        assert!(scopes.is_empty());
    }

    #[tokio::test]
    async fn the_other_holder_of_an_ambiguous_scope_is_reported() {
        let manager = manager_with_ambiguous_scope_holder();

        let scopes = manager
            .other_projects_s3_scopes(41, 8)
            .await
            .expect("scopes");

        let holders = scopes.get("acme-web-prod").expect("slug is held");
        assert_eq!(
            holders.iter().collect::<Vec<_>>(),
            vec![&("acme".to_string(), "web-prod".to_string())]
        );
    }

    #[test]
    fn managed_s3_errors_map_to_typed_service_errors() {
        use crate::externalsvc::ManagedS3Error;
        let validation = ExternalServiceManager::managed_s3_error(
            7,
            ManagedS3Error::InvalidBucketName {
                bucket: "Bad_Name".to_string(),
                reason: "uppercase".to_string(),
            },
        );
        assert!(matches!(
            validation,
            ExternalServiceError::ParameterValidationFailed { service_id: 7, .. }
        ));

        let internal = ExternalServiceManager::managed_s3_error(
            7,
            ManagedS3Error::Admin {
                backend: "garage",
                service: "files".to_string(),
                operation: "apply layout".to_string(),
                reason: "connection refused".to_string(),
            },
        );
        match internal {
            ExternalServiceError::InternalError { reason } => {
                assert!(
                    reason.contains("service 7") && reason.contains("apply layout"),
                    "{reason}"
                );
            }
            other => panic!("expected an internal error, got {other:?}"),
        }
    }

    #[test]
    fn migrating_a_service_into_itself_is_rejected_before_any_lookup() {
        let manager = mock_service_manager(vec![]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let err = runtime
            .block_on(manager.migrate_s3_service(5, 5))
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::ParameterValidationFailed { service_id: 5, .. }
        ));
    }
}
