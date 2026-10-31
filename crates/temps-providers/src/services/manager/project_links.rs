// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Links between services and projects.

use crate::services::*;
use chrono::Utc;
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::{BTreeMap, BTreeSet};
use temps_entities::{external_services, project_services, projects};
use tracing::error;

fn validate_creator_claim(
    service_id: i32,
    created_by_user_id: Option<i32>,
    already_linked: bool,
    claim_user_id: i32,
) -> Result<(), ExternalServiceError> {
    if already_linked || created_by_user_id != Some(claim_user_id) {
        Err(ExternalServiceError::ServiceClaimDenied { service_id })
    } else {
        Ok(())
    }
}

impl ExternalServiceManager {
    /// Resolve project links for each requested service in a fixed number of
    /// queries. Every requested service must exist; unlinked services are
    /// returned with an empty project list so authorization callers can deny
    /// them explicitly instead of confusing them with unknown IDs.
    pub async fn project_scopes_for_services(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ExternalServiceProjectScope>, ExternalServiceError> {
        let unique_ids: BTreeSet<i32> = service_ids.iter().copied().collect();
        if unique_ids.is_empty() {
            return Ok(Vec::new());
        }
        let requested_ids: Vec<i32> = unique_ids.iter().copied().collect();

        let existing_services = external_services::Entity::find()
            .filter(external_services::Column::Id.is_in(requested_ids.clone()))
            .all(self.db.as_ref())
            .await?;
        let creators_by_service: BTreeMap<i32, Option<i32>> = existing_services
            .into_iter()
            .map(|service| (service.id, service.created_by_user_id))
            .collect();
        let existing_ids: BTreeSet<i32> = creators_by_service.keys().copied().collect();
        if let Some(id) = unique_ids.difference(&existing_ids).next().copied() {
            return Err(ExternalServiceError::ServiceNotFound { id });
        }

        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.is_in(requested_ids))
            .all(self.db.as_ref())
            .await?;
        let mut projects_by_service: BTreeMap<i32, BTreeSet<i32>> = unique_ids
            .into_iter()
            .map(|service_id| (service_id, BTreeSet::new()))
            .collect();
        for link in links {
            if let Some(project_ids) = projects_by_service.get_mut(&link.service_id) {
                project_ids.insert(link.project_id);
            }
        }

        Ok(projects_by_service
            .into_iter()
            .map(|(service_id, project_ids)| ExternalServiceProjectScope {
                service_id,
                project_ids: project_ids.into_iter().collect(),
                created_by_user_id: creators_by_service.get(&service_id).copied().flatten(),
            })
            .collect())
    }

    pub async fn link_service_to_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        self.link_service_to_project_with_claim(service_id_val, project_id_val, None)
            .await
    }

    /// Link a service and atomically consume its one-time creator claim.
    ///
    /// `claim_user_id` is supplied only when authorization relied on an
    /// unlinked service's creator marker. The row lock makes that decision and
    /// the link insertion one atomic operation: a concurrent request cannot
    /// reuse the same bootstrap grant, and unlinking later cannot restore it.
    pub async fn link_service_to_project_with_claim(
        &self,
        service_id_val: i32,
        project_id_val: i32,
        claim_user_id: Option<i32>,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        self.link_service_to_project_with_provisioning(
            service_id_val,
            project_id_val,
            claim_user_id,
            DatabaseProvisioningConfig::default(),
        )
        .await
    }

    pub async fn link_service_to_project_with_provisioning(
        &self,
        service_id_val: i32,
        project_id_val: i32,
        claim_user_id: Option<i32>,
        provisioning: DatabaseProvisioningConfig,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        let claims = claim_user_id
            .map(|user_id| BTreeMap::from([(service_id_val, user_id)]))
            .unwrap_or_default();
        let provisioning_by_service = BTreeMap::from([(service_id_val, provisioning)]);
        let mut links = self
            .link_services_to_project_transactionally(
                &[service_id_val],
                project_id_val,
                &claims,
                &provisioning_by_service,
            )
            .await?;
        let link = links
            .pop()
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "linking service {} to project {} produced no link",
                    service_id_val, project_id_val
                ),
            })?;
        let service_info = self.get_service_info(service_id_val).await?;

        // Fetch project metadata
        let project = projects::Entity::find_by_id(link.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound {
                id: link.project_id,
            })?;

        Ok(ProjectServiceInfo {
            id: link.id,
            project: ProjectInfo {
                id: project.id,
                slug: project.slug,
                created_at: project.created_at.to_rfc3339(),
            },
            service: service_info,
            database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                &link.database_provisioning_mode,
                link.service_id,
                link.project_id,
            )?,
            custom_database_name: link.custom_database_name,
        })
    }

    /// Link every selected service and consume creator claims in one transaction.
    ///
    /// Project creation uses this bulk operation so a validation or insert failure
    /// for a later database cannot leave an earlier database unlinked with its
    /// one-time creator claim already consumed.
    pub async fn link_services_to_project_with_claims(
        &self,
        service_ids: &[i32],
        project_id: i32,
        claims: &BTreeMap<i32, i32>,
    ) -> Result<(), ExternalServiceError> {
        self.link_services_to_project_transactionally(
            service_ids,
            project_id,
            claims,
            &BTreeMap::new(),
        )
        .await?;
        Ok(())
    }

    async fn link_services_to_project_transactionally(
        &self,
        service_ids: &[i32],
        project_id: i32,
        claims: &BTreeMap<i32, i32>,
        provisioning_by_service: &BTreeMap<i32, DatabaseProvisioningConfig>,
    ) -> Result<Vec<project_services::Model>, ExternalServiceError> {
        let mut ordered_service_ids = service_ids.to_vec();
        ordered_service_ids.sort_unstable();
        ordered_service_ids.dedup();
        if ordered_service_ids.is_empty() {
            return Ok(Vec::new());
        }

        let claims = claims.clone();
        let provisioning_by_service = provisioning_by_service.clone();
        self.db
            .transaction::<_, Vec<project_services::Model>, ExternalServiceError>(|txn| {
                Box::pin(async move {
                    let services = external_services::Entity::find()
                        .filter(external_services::Column::Id.is_in(ordered_service_ids.clone()))
                        .order_by_asc(external_services::Column::Id)
                        .lock(LockType::Update)
                        .all(txn)
                        .await?;
                    if services.len() != ordered_service_ids.len() {
                        let found_ids = services
                            .iter()
                            .map(|service| service.id)
                            .collect::<BTreeSet<_>>();
                        let missing_id = ordered_service_ids
                            .iter()
                            .find(|service_id| !found_ids.contains(service_id))
                            .copied()
                            .unwrap_or_default();
                        return Err(ExternalServiceError::ServiceNotFound { id: missing_id });
                    }

                    projects::Entity::find_by_id(project_id)
                        .lock(LockType::Update)
                        .one(txn)
                        .await?
                        .ok_or(ExternalServiceError::ProjectNotFound { id: project_id })?;

                    let selected_links = project_services::Entity::find()
                        .filter(
                            project_services::Column::ServiceId.is_in(ordered_service_ids.clone()),
                        )
                        .all(txn)
                        .await?;
                    let already_linked_ids = selected_links
                        .iter()
                        .map(|link| link.service_id)
                        .collect::<BTreeSet<_>>();
                    for service in &services {
                        if let Some(user_id) = claims.get(&service.id) {
                            validate_creator_claim(
                                service.id,
                                service.created_by_user_id,
                                already_linked_ids.contains(&service.id),
                                *user_id,
                            )?;
                        }
                    }

                    let existing_links = project_services::Entity::find()
                        .filter(project_services::Column::ProjectId.eq(project_id))
                        .all(txn)
                        .await?;
                    let existing_service_ids = existing_links
                        .into_iter()
                        .map(|link| link.service_id)
                        .collect::<Vec<_>>();
                    let mut linked_service_types = if existing_service_ids.is_empty() {
                        BTreeSet::new()
                    } else {
                        external_services::Entity::find()
                            .filter(external_services::Column::Id.is_in(existing_service_ids))
                            .all(txn)
                            .await?
                            .into_iter()
                            .map(|service| service.service_type)
                            .collect::<BTreeSet<_>>()
                    };
                    for service in &services {
                        if !linked_service_types.insert(service.service_type.clone()) {
                            return Err(ExternalServiceError::DuplicateServiceType {
                                project_id,
                                service_type: service.service_type.clone(),
                            });
                        }
                        provisioning_by_service
                            .get(&service.id)
                            .cloned()
                            .unwrap_or_default()
                            .validate(service.id, project_id, &service.service_type)?;
                    }

                    let now = Utc::now();
                    let mut links = Vec::with_capacity(services.len());
                    for service in services {
                        let provisioning = provisioning_by_service
                            .get(&service.id)
                            .cloned()
                            .unwrap_or_default();
                        let link = project_services::ActiveModel {
                            project_id: Set(project_id),
                            service_id: Set(service.id),
                            database_provisioning_mode: Set(provisioning.mode.as_str().to_string()),
                            custom_database_name: Set(provisioning.custom_database_name),
                            created_at: Set(now),
                            updated_at: Set(now),
                            ..Default::default()
                        }
                        .insert(txn)
                        .await?;
                        links.push(link);

                        if service.created_by_user_id.is_some() {
                            let mut service_update: external_services::ActiveModel = service.into();
                            service_update.created_by_user_id = Set(None);
                            service_update.update(txn).await?;
                        }
                    }

                    Ok(links)
                })
            })
            .await
            .map_err(ExternalServiceError::from)
    }

    /// Check a target before provisioning a new service that should be linked
    /// to it. This prevents starting a database container only to discover
    /// that the project is missing or already has this service type.
    pub async fn validate_service_link_target(
        &self,
        project_id_val: i32,
        service_type: &str,
    ) -> Result<(), ExternalServiceError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Check for duplicate service type
        // Get all existing project_services for this project
        let existing_links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        // Check if any existing service has the same type
        for existing_link in existing_links {
            let existing_service = self.get_service(existing_link.service_id).await?;
            if existing_service.service_type == service_type {
                return Err(ExternalServiceError::DuplicateServiceType {
                    project_id: project_id_val,
                    service_type: service_type.to_string(),
                });
            }
        }

        Ok(())
    }

    pub async fn unlink_service_from_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<(), ExternalServiceError> {
        // Verify service exists
        let service = self.get_service(service_id_val).await?;

        let link = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            })?;

        // Credentials minted for the project's environments must not outlive
        // the link, so the link is kept when they cannot be revoked: the
        // operator fixes the cause (a stopped storage engine, an unreachable
        // worker) and unlinks again, rather than leaving working keys behind
        // with nothing left to retry them.
        if let Err(error) = self.revoke_managed_s3_credentials(&service, &link).await {
            error!(
                service_id = service_id_val,
                project_id = project_id_val,
                "Failed to revoke managed S3 credentials while unlinking: {}",
                error
            );
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Could not revoke the storage credentials of project {} on service {}, so the link was kept; make sure the service is running and reachable, then unlink again: {}",
                    project_id_val, service_id_val, error
                ),
            });
        }

        // Delete the link
        let deleted = project_services::Entity::delete_many()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .exec(self.db.as_ref())
            .await?;

        if deleted.rows_affected == 0 {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        Ok(())
    }

    pub async fn list_service_projects(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify service exists and get service info
        let service_info = self.get_service_info(service_id_val).await?;

        // Get all project links for this service
        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo with project metadata
        let mut project_services_list = Vec::new();
        for link in links {
            // Fetch project metadata
            let project = projects::Entity::find_by_id(link.project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::ProjectNotFound {
                    id: link.project_id,
                })?;

            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug,
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info.clone(),
                database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                    &link.database_provisioning_mode,
                    link.service_id,
                    link.project_id,
                )?,
                custom_database_name: link.custom_database_name,
            });
        }

        Ok(project_services_list)
    }

    pub async fn list_service_projects_paginated(
        &self,
        service_id_val: i32,
        page: u64,
        page_size: u64,
        hidden_project_ids: &[i32],
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify service exists and get service info
        let service_info = self.get_service_info(service_id_val).await?;

        // Filter hidden projects before pagination so authorized callers get a
        // full page without exposing tenant metadata or producing sparse pages.
        let mut query = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id_val));
        if !hidden_project_ids.is_empty() {
            query = query.filter(
                project_services::Column::ProjectId.is_not_in(hidden_project_ids.iter().copied()),
            );
        }
        let links = query
            .find_also_related(projects::Entity)
            .order_by_desc(project_services::Column::Id)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        links
            .into_iter()
            .map(|(link, project)| {
                let project = project.ok_or(ExternalServiceError::ProjectNotFound {
                    id: link.project_id,
                })?;
                Ok(ProjectServiceInfo {
                    id: link.id,
                    project: ProjectInfo {
                        id: project.id,
                        slug: project.slug,
                        created_at: project.created_at.to_rfc3339(),
                    },
                    service: service_info.clone(),
                    database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                        &link.database_provisioning_mode,
                        link.service_id,
                        link.project_id,
                    )?,
                    custom_database_name: link.custom_database_name,
                })
            })
            .collect()
    }

    pub async fn list_project_services(
        &self,
        project_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify project exists and fetch its metadata
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all service links for this project
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo with service details
        let mut project_services_list = Vec::new();
        for link in links {
            let service_info = self.get_service_info(link.service_id).await?;
            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug.clone(),
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info,
                database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                    &link.database_provisioning_mode,
                    link.service_id,
                    link.project_id,
                )?,
                custom_database_name: link.custom_database_name,
            });
        }

        Ok(project_services_list)
    }

    pub async fn list_project_services_paginated(
        &self,
        project_id_val: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify project exists and fetch its metadata
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get paginated service links for this project
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .order_by_desc(project_services::Column::Id)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        // Convert to ProjectServiceInfo with service details
        let mut project_services_list = Vec::new();
        for link in links {
            let service_info = self.get_service_info(link.service_id).await?;
            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug.clone(),
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info,
                database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                    &link.database_provisioning_mode,
                    link.service_id,
                    link.project_id,
                )?,
                custom_database_name: link.custom_database_name,
            });
        }

        Ok(project_services_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::*;
    #[cfg(feature = "docker-tests")]
    use bollard::Docker;
    use chrono::Utc;
    #[cfg(feature = "docker-tests")]
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    #[cfg(feature = "docker-tests")]
    use std::collections::BTreeMap;
    use std::sync::Arc;
    #[cfg(feature = "docker-tests")]
    use temps_core::EncryptionService;
    use temps_entities::project_services;
    #[cfg(feature = "docker-tests")]
    use temps_entities::{external_services, projects};

    #[test]
    fn creator_claim_is_one_time_and_cannot_reappear_after_unlink() {
        assert!(validate_creator_claim(7, Some(42), false, 42).is_ok());
        assert!(matches!(
            validate_creator_claim(7, Some(42), true, 42),
            Err(ExternalServiceError::ServiceClaimDenied { service_id: 7 })
        ));
        assert!(matches!(
            validate_creator_claim(7, None, false, 42),
            Err(ExternalServiceError::ServiceClaimDenied { service_id: 7 })
        ));
        assert!(matches!(
            validate_creator_claim(7, Some(99), false, 42),
            Err(ExternalServiceError::ServiceClaimDenied { service_id: 7 })
        ));
    }

    #[tokio::test]
    async fn database_provisioning_link_persists_selected_mode_and_name() {
        for (mode, name) in [
            (DatabaseProvisioningMode::Project, None),
            (
                DatabaseProvisioningMode::Custom,
                Some("shared_catalog".to_string()),
            ),
        ] {
            let mut service = encrypted_service_model(71, serde_json::json!({}));
            service.created_by_user_id = None;
            let project = environment_preview_project(10);
            let link = project_services::Model {
                id: 9,
                project_id: 10,
                service_id: 71,
                database_provisioning_mode: mode.as_str().to_string(),
                custom_database_name: name.clone(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            let db = Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                    .append_query_results([vec![service.clone()]])
                    .append_query_results([vec![project.clone()]])
                    .append_query_results([Vec::<project_services::Model>::new(), Vec::new()])
                    .append_query_results([vec![link]])
                    .append_query_results([vec![service]])
                    .append_query_results([vec![project]])
                    .into_connection(),
            );
            let manager = mock_service_manager_with_db(Arc::clone(&db));
            let result = manager
                .link_service_to_project_with_provisioning(
                    71,
                    10,
                    None,
                    DatabaseProvisioningConfig {
                        mode,
                        custom_database_name: name.clone(),
                    },
                )
                .await
                .expect("link with selected database provisioning");
            assert_eq!(result.database_provisioning_mode, mode);
            assert_eq!(result.custom_database_name, name);
            drop(manager);
            let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("database still owned"));
            let log = db.into_transaction_log();
            let insert = log
                .iter()
                .flat_map(|transaction| transaction.statements())
                .find(|statement| {
                    statement
                        .sql
                        .starts_with("INSERT INTO \"project_services\"")
                })
                .expect("link insert executed")
                .to_string();
            assert!(insert.contains("database_provisioning_mode"));
            assert!(insert.contains(mode.as_str()));
            assert!(insert.contains("custom_database_name"));
            if let Some(name) = name {
                assert!(insert.contains(&name));
            }
        }
    }

    #[tokio::test]
    async fn database_provisioning_invalid_link_does_not_consume_creator_claim() {
        let mut service = encrypted_service_model(71, serde_json::json!({}));
        service.created_by_user_id = Some(42);
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![service]])
                .append_query_results([vec![environment_preview_project(10)]])
                .append_query_results([Vec::<project_services::Model>::new(), Vec::new()])
                .into_connection(),
        );
        let manager = mock_service_manager_with_db(Arc::clone(&db));
        let error = manager
            .link_service_to_project_with_provisioning(
                71,
                10,
                Some(42),
                DatabaseProvisioningConfig {
                    mode: DatabaseProvisioningMode::Custom,
                    custom_database_name: Some("bad-name".to_string()),
                },
            )
            .await
            .expect_err("invalid custom database must reject link");
        assert!(matches!(
            error,
            ExternalServiceError::InvalidDatabaseProvisioning {
                service_id: 71,
                project_id: 10,
                ..
            }
        ));
        drop(manager);
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("database still owned"));
        let log = db.into_transaction_log();
        assert!(
            log.iter()
                .flat_map(|transaction| transaction.statements())
                .all(|statement| !statement.sql.starts_with("INSERT")
                    && !statement.sql.starts_with("UPDATE")),
            "validation must happen before link creation or claim consumption"
        );
    }

    #[tokio::test]
    async fn project_scopes_for_services_are_complete_deduplicated_and_sorted() {
        let service_a = encrypted_service_model(17, serde_json::json!({}));
        let mut service_b = encrypted_service_model(23, serde_json::json!({}));
        service_b.created_by_user_id = Some(42);
        let now = Utc::now();
        let links = vec![
            project_services::Model {
                id: 1,
                project_id: 9,
                service_id: 17,
                database_provisioning_mode: "project_environment".to_string(),
                custom_database_name: None,
                created_at: now,
                updated_at: now,
            },
            project_services::Model {
                id: 2,
                project_id: 4,
                service_id: 17,
                database_provisioning_mode: "project_environment".to_string(),
                custom_database_name: None,
                created_at: now,
                updated_at: now,
            },
        ];
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![service_b, service_a]])
                .append_query_results([links])
                .into_connection(),
        ));

        let scopes = manager
            .project_scopes_for_services(&[23, 17, 17])
            .await
            .expect("service scopes should resolve");

        assert_eq!(
            scopes,
            vec![
                ExternalServiceProjectScope {
                    service_id: 17,
                    project_ids: vec![4, 9],
                    created_by_user_id: None,
                },
                ExternalServiceProjectScope {
                    service_id: 23,
                    project_ids: Vec::new(),
                    created_by_user_id: Some(42),
                },
            ]
        );
    }

    #[tokio::test]
    async fn project_scopes_for_services_reject_unknown_ids() {
        let manager = mock_service_manager(vec![Vec::new()]);

        let error = manager
            .project_scopes_for_services(&[404])
            .await
            .expect_err("unknown services must not be returned as ownerless");

        assert!(matches!(
            error,
            ExternalServiceError::ServiceNotFound { id: 404 }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn bulk_link_failure_preserves_every_creator_claim() {
        use temps_entities::preset::Preset;
        use temps_entities::users;

        let (manager, test_db) = setup_test_manager_or_skip!();
        let now = Utc::now();
        let user = users::ActiveModel {
            name: Set("Database Creator".to_string()),
            email: Set(format!(
                "bulk-link-creator-{}@test.local",
                now.timestamp_nanos_opt().unwrap_or(0)
            )),
            email_verified: Set(true),
            mfa_enabled: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .expect("insert creator");
        let project = projects::ActiveModel {
            name: Set("atomic database links".to_string()),
            preset: Set(Preset::Static),
            slug: Set(format!("atomic-database-links-{}", now.timestamp_millis())),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .expect("insert project");

        let mut service_ids = Vec::new();
        for suffix in ["one", "two"] {
            let service = external_services::ActiveModel {
                name: Set(format!("atomic-postgres-{suffix}")),
                service_type: Set("postgres".to_string()),
                version: Set(Some("17".to_string())),
                status: Set("creating".to_string()),
                slug: Set(Some(format!("atomic-postgres-{suffix}"))),
                created_by_user_id: Set(Some(user.id)),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(test_db.db.as_ref())
            .await
            .expect("insert claimed service");
            service_ids.push(service.id);
        }
        let claims = service_ids
            .iter()
            .copied()
            .map(|service_id| (service_id, user.id))
            .collect::<BTreeMap<_, _>>();

        let error = manager
            .link_services_to_project_with_claims(&service_ids, project.id, &claims)
            .await
            .expect_err("duplicate database types must reject the whole bulk link");
        assert!(matches!(
            error,
            ExternalServiceError::DuplicateServiceType { .. }
        ));

        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(test_db.db.as_ref())
            .await
            .expect("query project links");
        assert!(links.is_empty(), "a failed bulk link must create no links");

        let services = external_services::Entity::find()
            .filter(external_services::Column::Id.is_in(service_ids))
            .all(test_db.db.as_ref())
            .await
            .expect("query claimed services");
        assert_eq!(services.len(), 2);
        assert!(services
            .iter()
            .all(|service| service.created_by_user_id == Some(user.id)));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_prevent_duplicate_service_type_linking() {
        use temps_entities::preset::Preset;
        use temps_entities::{external_services, project_services, projects};

        let (_manager, test_db) = setup_test_manager_or_skip!();

        // Create a test project
        let project = projects::ActiveModel {
            name: Set("test-project-duplicate-services".to_string()),
            preset: Set(Preset::Static),
            slug: Set("test-project-duplicate".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        let project = project
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create project");
        let project_id = project.id;

        // Create first PostgreSQL service (directly in database, not via manager)
        let service_pg1 = external_services::ActiveModel {
            name: Set("test-postgres-1".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("16".to_string())),
            status: Set("active".to_string()),
            slug: Set(Some("test-postgres-1".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let service_pg1 = service_pg1
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create first service");

        // Create second PostgreSQL service
        let service_pg2 = external_services::ActiveModel {
            name: Set("test-postgres-2".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("16".to_string())),
            status: Set("active".to_string()),
            slug: Set(Some("test-postgres-2".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let service_pg2 = service_pg2
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create second service");

        // Create an ExternalServiceManager for testing
        let encryption_key = "test_encryption_key_1234567890ab";
        let encryption_service = Arc::new(EncryptionService::new(encryption_key).unwrap());
        let docker = Arc::new(Docker::connect_with_local_defaults().ok().unwrap());
        let dns_registry = Arc::new(temps_dns::DnsRegistry::new(test_db.db.clone()));
        let manager = ExternalServiceManager::new(
            test_db.db.clone(),
            encryption_service,
            docker,
            dns_registry,
        );

        // Link first PostgreSQL service to project
        let result_link1 = manager
            .link_service_to_project(service_pg1.id, project_id)
            .await;
        assert!(
            result_link1.is_ok(),
            "Failed to link first PostgreSQL service: {:?}",
            result_link1.err()
        );

        // Try to link second PostgreSQL service (should fail due to duplicate type)
        let result_link2 = manager
            .link_service_to_project(service_pg2.id, project_id)
            .await;

        assert!(
            result_link2.is_err(),
            "Expected linking second PostgreSQL service to fail due to duplicate service type"
        );

        // Verify it's the correct error type
        match result_link2 {
            Err(ExternalServiceError::DuplicateServiceType {
                project_id: pid,
                service_type,
            }) => {
                assert_eq!(pid, project_id);
                assert_eq!(service_type, "postgres");
            }
            _ => panic!(
                "Expected DuplicateServiceType error, got: {:?}",
                result_link2
            ),
        }

        // Verify first link was created by checking the database
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .all(test_db.db.as_ref())
            .await
            .expect("Failed to query links");

        assert_eq!(links.len(), 1, "Expected exactly one service link");
        assert_eq!(links[0].service_id, service_pg1.id);
    }
}
