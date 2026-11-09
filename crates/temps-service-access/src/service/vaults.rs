// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::vault_types::*;
use std::collections::{BTreeMap, BTreeSet};
use temps_entities::{access_bundles as bundles, access_vaults as vaults};

fn validate_selection(
    name: &mut String,
    ids: &[String],
    minimum: usize,
) -> Result<(), AccessError> {
    *name = name.trim().into();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(invalid("name", "must contain 1–80 printable characters"));
    }
    if minimum > 0 && ids.len() > 16 {
        return Err(invalid(
            "service_ids",
            "a bundle supports at most 16 services",
        ));
    }
    if ids.len() < minimum
        || ids.len() > 100
        || ids.iter().any(|id| id.is_empty() || id.len() > 128)
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err(invalid(
            "service_ids",
            "select distinct services, at most 100; a bundle needs at least one",
        ));
    }
    Ok(())
}
fn check_version(id: &str, actual: i32, expected: Option<i32>) -> Result<i32, AccessError> {
    if expected != Some(actual) || actual == i32::MAX {
        return Err(AccessError::Conflict {
            id: id.into(),
            version: expected.unwrap_or_default(),
        });
    }
    Ok(actual + 1)
}

impl ServiceAccessService {
    pub(super) async fn vault<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
        id: &str,
        lock: bool,
    ) -> Result<vaults::Model, AccessError> {
        let query = vaults::Entity::find_by_id(id).filter(vaults::Column::ProjectId.eq(project_id));
        let query = if lock { query.lock_exclusive() } else { query };
        query
            .one(db)
            .await
            .map_err(|e| AccessError::database("read vault", project_id, e))?
            .ok_or_else(|| not_found("vault", id.into(), project_id))
    }
    async fn selected_services<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
        ids: &[String],
    ) -> Result<Vec<AccessServiceResponse>, AccessError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = services::Entity::find()
            .filter(services::Column::ProjectId.eq(project_id))
            .filter(services::Column::Id.is_in(ids.to_vec()))
            .order_by_desc(services::Column::CreatedAt)
            .order_by_desc(services::Column::Id)
            .all(db)
            .await
            .map_err(|e| AccessError::database("read vault services", project_id, e))?;
        if let Some(id) = ids.iter().find(|id| !rows.iter().any(|row| &row.id == *id)) {
            return Err(not_found("service", id.clone(), project_id));
        }
        rows.into_iter().map(service_response).collect()
    }
    pub async fn list_vaults(
        &self,
        project_id: i32,
        query: AccessPageQuery,
    ) -> Result<AccessVaultList, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let (page, page_size) = query.normalized();
        let pages = vaults::Entity::find()
            .filter(vaults::Column::ProjectId.eq(project_id))
            .order_by_desc(vaults::Column::CreatedAt)
            .order_by_desc(vaults::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count vaults", project_id, e))?;
        let rows = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list vaults", project_id, e))?;
        // One bounded batch for every card, never a query per vault.
        let ids: Vec<_> = rows
            .iter()
            .flat_map(|row| row.service_ids.iter().take(4).cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let previews: BTreeMap<_, _> = self
            .selected_services(self.db.as_ref(), project_id, &ids)
            .await?
            .into_iter()
            .map(|v| (v.id.clone(), v))
            .collect();
        let items = rows
            .into_iter()
            .map(|row| AccessVaultOverview {
                services: row
                    .service_ids
                    .iter()
                    .take(4)
                    .filter_map(|id| previews.get(id).cloned())
                    .collect(),
                vault: row.into(),
            })
            .collect();
        Ok(AccessVaultList {
            items,
            total,
            page,
            page_size,
        })
    }
    pub async fn get_vault(
        &self,
        project_id: i32,
        id: &str,
    ) -> Result<AccessVaultDetail, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let vault = self.vault(self.db.as_ref(), project_id, id, false).await?;
        let services = self
            .selected_services(self.db.as_ref(), project_id, &vault.service_ids)
            .await?;
        Ok(AccessVaultDetail {
            vault: vault.into(),
            services,
        })
    }
    pub async fn save_vault(
        &self,
        project_id: i32,
        id: Option<String>,
        mut request: SaveAccessVault,
    ) -> Result<AccessVaultResponse, AccessError> {
        validate_selection(&mut request.name, &request.service_ids, 0)?;
        if id.is_some() != request.expected_version.is_some() {
            return Err(invalid("expected_version", "required only for updates"));
        }
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin vault save", project_id, e))?;
        self.project(&tx, project_id).await?;
        let updating = id.is_some();
        let mut model = if let Some(id) = id {
            let current = self.vault(&tx, project_id, &id, true).await?;
            let version = check_version(&id, current.version, request.expected_version)?;
            // Bundle writers take the same vault lock. Removing a referenced
            // service cannot silently broaden or invalidate a saved bundle.
            // PostgreSQL checks the array subset in the database; only one
            // conflicting ID is read, regardless of the number of bundles.
            use sea_orm::sea_query::Expr;
            let conflict = bundles::Entity::find()
                .filter(bundles::Column::ProjectId.eq(project_id))
                .filter(bundles::Column::VaultId.eq(&id))
                .filter(Expr::cust_with_values(
                    "NOT (service_ids <@ $1::text[])",
                    [sea_orm::Value::from(request.service_ids.clone())],
                ))
                .select_only()
                .column(bundles::Column::Id)
                .into_tuple::<String>()
                .one(&tx)
                .await
                .map_err(|e| {
                    AccessError::database("check vault bundle references", project_id, e)
                })?;
            if conflict.is_some() {
                return Err(invalid(
                    "service_ids",
                    "remove the service from its access bundles before removing it from the vault",
                ));
            }
            let mut model = current.into_active_model();
            model.version = Set(version);
            model
        } else {
            vaults::ActiveModel {
                id: Set(format!("vault_{}", uuid::Uuid::new_v4().simple())),
                project_id: Set(project_id),
                version: Set(1),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
        };
        self.selected_services(&tx, project_id, &request.service_ids)
            .await?;
        model.name = Set(request.name);
        model.service_ids = Set(request.service_ids);
        model.updated_at = Set(Utc::now());
        let row = if updating {
            model.update(&tx).await
        } else {
            model.insert(&tx).await
        }
        .map_err(|e| AccessError::database("save vault", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit vault save", project_id, e))?;
        Ok(row.into())
    }
    pub async fn list_bundles(
        &self,
        project_id: i32,
        vault_id: &str,
        query: AccessPageQuery,
    ) -> Result<AccessBundleList, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        self.vault(self.db.as_ref(), project_id, vault_id, false)
            .await?;
        let (page, page_size) = query.normalized();
        let pages = bundles::Entity::find()
            .filter(bundles::Column::ProjectId.eq(project_id))
            .filter(bundles::Column::VaultId.eq(vault_id))
            .order_by_desc(bundles::Column::CreatedAt)
            .order_by_desc(bundles::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count bundles", project_id, e))?;
        let items = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list bundles", project_id, e))?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(AccessBundleList {
            items,
            total,
            page,
            page_size,
        })
    }
    pub async fn save_bundle(
        &self,
        project_id: i32,
        vault_id: &str,
        id: Option<String>,
        mut request: SaveAccessBundle,
    ) -> Result<AccessBundleResponse, AccessError> {
        validate_selection(&mut request.name, &request.service_ids, 1)?;
        if !(60..=86400).contains(&request.ttl_seconds) {
            return Err(invalid("ttl_seconds", "must be between 60 and 86400"));
        }
        if id.is_some() != request.expected_version.is_some() {
            return Err(invalid("expected_version", "required only for updates"));
        }
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin bundle save", project_id, e))?;
        self.project(&tx, project_id).await?;
        let vault = self.vault(&tx, project_id, vault_id, true).await?;
        if request
            .service_ids
            .iter()
            .any(|id| !vault.service_ids.contains(id))
        {
            return Err(invalid(
                "service_ids",
                "every service must belong to this vault",
            ));
        }
        let selected = self
            .selected_services(&tx, project_id, &request.service_ids)
            .await?;
        if selected
            .iter()
            .any(|s| request.ttl_seconds > s.max_ttl_seconds)
        {
            return Err(invalid(
                "ttl_seconds",
                "exceeds a selected service's maximum lifetime",
            ));
        }
        let updating = id.is_some();
        let mut model = if let Some(id) = id {
            let current = bundles::Entity::find_by_id(&id)
                .filter(bundles::Column::ProjectId.eq(project_id))
                .filter(bundles::Column::VaultId.eq(vault_id))
                .lock_exclusive()
                .one(&tx)
                .await
                .map_err(|e| AccessError::database("lock bundle", project_id, e))?
                .ok_or_else(|| not_found("bundle", id.clone(), project_id))?;
            let version = check_version(&id, current.version, request.expected_version)?;
            let mut model = current.into_active_model();
            model.version = Set(version);
            model
        } else {
            bundles::ActiveModel {
                id: Set(format!("bundle_{}", uuid::Uuid::new_v4().simple())),
                project_id: Set(project_id),
                vault_id: Set(vault_id.into()),
                version: Set(1),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
        };
        model.name = Set(request.name);
        model.service_ids = Set(request.service_ids);
        model.ttl_seconds = Set(request.ttl_seconds);
        model.updated_at = Set(Utc::now());
        let row = if updating {
            model.update(&tx).await
        } else {
            model.insert(&tx).await
        }
        .map_err(|e| AccessError::database("save bundle", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit bundle save", project_id, e))?;
        Ok(row.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selections_are_bounded_and_unique() {
        for ids in [
            vec!["a".into(), "a".into()],
            vec!["".into()],
            (0..101).map(|n| n.to_string()).collect(),
        ] {
            assert!(validate_selection(&mut "Vault".into(), &ids, 0).is_err());
        }
        assert!(validate_selection(&mut "Vault".into(), &[], 0).is_ok());
        assert!(validate_selection(&mut "Bundle".into(), &[], 1).is_err());
        let ids: Vec<_> = (0..17).map(|n| format!("svc_{n}")).collect();
        assert!(validate_selection(&mut "Bundle".into(), &ids[..16], 1).is_ok());
        assert!(validate_selection(&mut "Bundle".into(), &ids, 1).is_err());
        assert!(validate_selection(&mut "Vault".into(), &ids, 0).is_ok());
        assert!(validate_selection(&mut " ".into(), &[], 0).is_err());
    }
    #[test]
    fn stale_writes_and_overflow_are_rejected() {
        assert!(check_version("vault_test", 2, Some(1)).is_err());
        assert!(check_version("vault_test", i32::MAX, Some(i32::MAX)).is_err());
        assert_eq!(check_version("vault_test", 2, Some(2)).unwrap(), 3);
    }
}

#[cfg(test)]
mod database_tests {
    use super::*;
    use crate::broker::UnavailableTokenlessBroker;
    #[tokio::test]
    async fn vaults_and_bundles_are_scoped_versioned_and_do_not_grant_access() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault integration: Docker unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9602,'Vault tests','repo','owner','/','main','nodejs','vault-tests',now(),now())").await.unwrap();
        let service = ServiceAccessService::new(
            db.clone(),
            super::super::tests::encryption(),
            Arc::new(UnavailableTokenlessBroker),
        );
        let vault = service
            .save_vault(
                9602,
                None,
                SaveAccessVault {
                    name: "Work".into(),
                    service_ids: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let definition = service
            .save_service(
                9602,
                None,
                SaveAccessService {
                    vault_id: Some(vault.id.clone()),
                    name: "API".into(),
                    policy: SandboxAccessPolicy {
                        provider: Some(AccessServiceProvider::Stripe),
                        origin: "https://api.example.com".into(),
                        path_prefixes: vec!["/v1".into()],
                        methods: vec![AccessMethod::Get],
                        github_repository: None,
                    },
                    max_ttl_seconds: 900,
                    enabled: true,
                    credential: None,
                    clear_credential: false,
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let detail = service.get_vault(9602, &vault.id).await.unwrap();
        assert_eq!(detail.vault.version, 2);
        assert_eq!(detail.services.len(), 1);
        assert!(service.get_vault(9603, &vault.id).await.is_err());
        assert!(service
            .save_vault(
                9602,
                Some(vault.id.clone()),
                SaveAccessVault {
                    name: "Stale".into(),
                    service_ids: vec![],
                    expected_version: Some(1)
                }
            )
            .await
            .is_err());
        let make = |ids: Vec<String>, ttl| SaveAccessBundle {
            name: "Read APIs".into(),
            service_ids: ids,
            ttl_seconds: ttl,
            expected_version: None,
        };
        assert!(service
            .save_bundle(9602, &vault.id, None, make(vec!["missing".into()], 60))
            .await
            .is_err());
        assert!(service
            .save_bundle(
                9602,
                &vault.id,
                None,
                make(vec![definition.id.clone()], 901)
            )
            .await
            .is_err());
        let bundle = service
            .save_bundle(
                9602,
                &vault.id,
                None,
                make(vec![definition.id.clone()], 900),
            )
            .await
            .unwrap();
        assert_eq!(bundle.service_ids, vec![definition.id.clone()]);
        assert!(service
            .save_vault(
                9602,
                Some(vault.id.clone()),
                SaveAccessVault {
                    name: "Work".into(),
                    service_ids: vec![],
                    expected_version: Some(2)
                }
            )
            .await
            .is_err());
        assert_eq!(
            service
                .list_vaults(
                    9602,
                    AccessPageQuery {
                        page: None,
                        page_size: None
                    }
                )
                .await
                .unwrap()
                .items[0]
                .services
                .len(),
            1
        );
        assert_eq!(
            service
                .list_bundles(
                    9602,
                    &vault.id,
                    AccessPageQuery {
                        page: None,
                        page_size: None
                    }
                )
                .await
                .unwrap()
                .total,
            1
        );
        assert_eq!(
            grants::Entity::find()
                .filter(grants::Column::ProjectId.eq(9602))
                .count(db.as_ref())
                .await
                .unwrap(),
            0
        );
        test.cleanup().await;
    }
}
