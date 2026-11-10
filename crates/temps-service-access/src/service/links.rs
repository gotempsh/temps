// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{sea_query::Expr, Condition, FromQueryResult};
use temps_entities::access_vault_links as links;

impl ServiceAccessService {
    pub(super) async fn require_standalone_service<C: ConnectionTrait>(
        &self,
        db: &C,
        service_id: &str,
    ) -> Result<(), AccessError> {
        let row = db
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT id FROM access_vaults WHERE service_ids @> ARRAY[$1] ORDER BY id LIMIT 1",
                [service_id.into()],
            ))
            .await
            .map_err(|e| {
                AccessError::resource_database("check legacy approval scope", service_id, e)
            })?;
        if let Some(row) = row {
            return Err(AccessError::VaultSessionRequired {
                service_id: service_id.into(),
                vault_id: row.try_get("", "id").map_err(|e| {
                    AccessError::resource_database("decode legacy approval scope", service_id, e)
                })?,
            });
        }
        Ok(())
    }

    pub async fn link_consumer(
        &self,
        vault_id: &str,
        request: crate::link_types::CreateVaultLink,
    ) -> Result<links::Model, AccessError> {
        if request.project_id.is_some() == request.sandbox_id.is_some() {
            return Err(invalid("consumer", "select exactly one project or sandbox"));
        }
        let sandbox_id = match request.sandbox_id {
            Some(id) => Some(self.owned_consumer(&id, 0, true).await?.0),
            None => None,
        };
        self.link_vault(vault_id, request.project_id, sandbox_id)
            .await
    }

    pub async fn retire_service(&self, service_id: &str) -> Result<(), AccessError> {
        let tx = self.db.begin().await.map_err(|e| {
            AccessError::resource_database("begin service retirement", service_id, e)
        })?;
        self.lock_vault_management(&tx).await?;
        // Match the approval lock order: vault before service. Keep history while
        // removing the service from active vault selections and retiring bundles.
        tx.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id FROM access_vaults WHERE service_ids @> ARRAY[$1] ORDER BY id FOR UPDATE",
            [service_id.into()],
        ))
        .await
        .map_err(|e| {
            AccessError::resource_database("lock vaults for service retirement", service_id, e)
        })?;
        services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::resource_database("read retiring service", service_id, e))?
            .ok_or_else(|| not_found("service", service_id.into(), None))?;
        for sql in [
            "UPDATE sandbox_service_grants g SET revoked_at=now(),version=version+1 WHERE revoked_at IS NULL AND (service_id=$1 OR EXISTS (SELECT 1 FROM access_sessions s JOIN access_bundles b ON b.id=s.bundle_id WHERE b.service_ids @> ARRAY[$1] AND s.grant_ids @> ARRAY[g.id]))",
            "UPDATE access_sessions s SET revoked_at=now() FROM access_bundles b WHERE b.id=s.bundle_id AND b.service_ids @> ARRAY[$1] AND s.revoked_at IS NULL",
            "UPDATE access_bundles SET retired_at=now() WHERE service_ids @> ARRAY[$1] AND retired_at IS NULL",
            "UPDATE access_vaults SET service_ids=array_remove(service_ids,$1),version=version+1,updated_at=now() WHERE service_ids @> ARRAY[$1]",
            "UPDATE sandbox_access_services SET retired_at=now(),enabled=false,credential_encrypted=NULL,version=version+1,updated_at=now() WHERE id=$1 AND retired_at IS NULL",
        ] {
            tx.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,sql,[service_id.into()])).await
                .map_err(|e| AccessError::resource_database("retire service and revoke dependent access", service_id, e))?;
        }
        tx.commit().await.map_err(|e| {
            AccessError::resource_database("commit service retirement", service_id, e)
        })?;
        Ok(())
    }

    pub async fn retire_vault_resource(
        &self,
        vault_id: &str,
        bundle_id: Option<&str>,
    ) -> Result<(), AccessError> {
        let tx =
            self.db.begin().await.map_err(|e| {
                AccessError::resource_database("begin vault retirement", vault_id, e)
            })?;
        self.lock_vault_management(&tx).await?;
        self.vault(&tx, None, vault_id, true).await?;
        if let Some(id) = bundle_id {
            temps_entities::access_bundles::Entity::find_by_id(id)
                .filter(temps_entities::access_bundles::Column::VaultId.eq(vault_id))
                .one(&tx)
                .await
                .map_err(|e| AccessError::resource_database("read retiring bundle", vault_id, e))?
                .ok_or_else(|| not_found("bundle", id.into(), None))?;
        }
        for sql in [
            "UPDATE sandbox_service_grants g SET revoked_at=now(),version=g.version+1 WHERE g.revoked_at IS NULL AND EXISTS (SELECT 1 FROM access_sessions s WHERE s.vault_id=$1 AND ($2::text IS NULL OR s.bundle_id=$2) AND s.grant_ids @> ARRAY[g.id])",
            "UPDATE access_sessions SET revoked_at=now() WHERE vault_id=$1 AND ($2::text IS NULL OR bundle_id=$2) AND revoked_at IS NULL",
            "UPDATE access_bundles SET retired_at=now() WHERE vault_id=$1 AND ($2::text IS NULL OR id=$2) AND retired_at IS NULL",
        ] {
            tx.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,sql,[vault_id.into(),bundle_id.map(str::to_owned).into()])).await
                .map_err(|e| AccessError::resource_database("retire vault resource and revoke access", vault_id, e))?;
        }
        if bundle_id.is_none() {
            links::Entity::update_many()
                .col_expr(links::Column::RevokedAt, Expr::value(Utc::now()))
                .filter(links::Column::VaultId.eq(vault_id))
                .filter(links::Column::RevokedAt.is_null())
                .exec(&tx)
                .await
                .map_err(|e| AccessError::resource_database("retire vault links", vault_id, e))?;
            temps_entities::access_vaults::Entity::update_many()
                .col_expr(
                    temps_entities::access_vaults::Column::RetiredAt,
                    Expr::value(Utc::now()),
                )
                .filter(temps_entities::access_vaults::Column::Id.eq(vault_id))
                .exec(&tx)
                .await
                .map_err(|e| AccessError::resource_database("retire vault", vault_id, e))?;
        }
        tx.commit()
            .await
            .map_err(|e| AccessError::resource_database("commit vault retirement", vault_id, e))?;
        Ok(())
    }

    pub async fn owned_consumer(
        &self,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<(i32, Option<i32>), AccessError> {
        let row = self
            .sandbox(self.db.as_ref(), None, sandbox_id, user_id, admin)
            .await?;
        Ok((row.id, row.project_id))
    }

    pub(super) async fn lock_vault_management<C: ConnectionTrait>(
        &self,
        db: &C,
    ) -> Result<(), AccessError> {
        db.execute_unprepared("SELECT pg_advisory_xact_lock(1876342201)")
            .await
            .map_err(|e| AccessError::database("serialize vault configuration", None, e))?;
        Ok(())
    }

    pub(super) async fn validate_linked_sources<C: ConnectionTrait>(
        &self,
        db: &C,
        vault_id: Option<&str>,
        service_id: Option<&str>,
    ) -> Result<(), AccessError> {
        let conflict = db.query_one(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "SELECT v.id AS vault_id, s.id AS service_id, s.project_id AS source_project_id, COALESCE(l.project_id,b.project_id) AS target_project_id FROM access_vault_links l JOIN access_vaults v ON v.id=l.vault_id JOIN sandbox_access_services s ON v.service_ids @> ARRAY[s.id] LEFT JOIN sandboxes b ON b.id=l.sandbox_id WHERE ($1::text IS NULL OR v.id=$1) AND ($2::text IS NULL OR s.id=$2) AND l.revoked_at IS NULL AND s.policy->'github_repository' IS NOT NULL AND s.policy->'github_repository' <> 'null'::jsonb AND (s.project_id IS NULL OR s.project_id IS DISTINCT FROM COALESCE(l.project_id,b.project_id)) LIMIT 1",[vault_id.map(str::to_owned).into(),service_id.map(str::to_owned).into()])).await
            .map_err(|e| AccessError::database("validate linked GitHub sources", None, e))?;
        if let Some(row) = conflict {
            let decode = |e| AccessError::database("decode conflicting GitHub source", None, e);
            return Err(AccessError::GitLinkConflict {
                vault_id: row.try_get("", "vault_id").map_err(decode)?,
                service_id: row.try_get("", "service_id").map_err(decode)?,
                source_project_id: row.try_get("", "source_project_id").map_err(decode)?,
                target_project_id: row.try_get("", "target_project_id").map_err(decode)?,
            });
        }
        Ok(())
    }

    pub async fn list_vault_links(
        &self,
        vault_id: &str,
        query: AccessPageQuery,
    ) -> Result<crate::link_types::VaultLinkList, AccessError> {
        self.vault(self.db.as_ref(), None, vault_id, false).await?;
        let (page, page_size) = query.normalized();
        let pages = links::Entity::find()
            .filter(links::Column::VaultId.eq(vault_id))
            .filter(links::Column::RevokedAt.is_null())
            .order_by_desc(links::Column::CreatedAt)
            .order_by_desc(links::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::resource_database("count vault links", vault_id, e))?;
        let items = crate::link_types::VaultLinkResponse::find_by_statement(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "SELECT l.*,COALESCE(p.name,b.name) AS consumer_name FROM access_vault_links l LEFT JOIN projects p ON p.id=l.project_id LEFT JOIN sandboxes b ON b.id=l.sandbox_id WHERE l.vault_id=$1 AND l.revoked_at IS NULL ORDER BY l.created_at DESC,l.id DESC LIMIT $2 OFFSET $3",
            [vault_id.into(),(page_size as i64).into(),(((page-1)*page_size) as i64).into()])).all(self.db.as_ref()).await
            .map_err(|e| AccessError::resource_database("list named vault links", vault_id, e))?;
        Ok(crate::link_types::VaultLinkList {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn session_eligibility(
        &self,
        vault_id: &str,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<crate::link_types::VaultLinkResponse, AccessError> {
        let sandbox = self
            .sandbox(self.db.as_ref(), None, sandbox_id, user_id, admin)
            .await?;
        Ok(self
            .covering_vault_link(vault_id, sandbox.id, sandbox.project_id)
            .await?
            .into())
    }

    /// Caller must authorize the consumer before exposing this result.
    pub async fn covering_vault_link(
        &self,
        vault_id: &str,
        sandbox_id: i32,
        project_id: Option<i32>,
    ) -> Result<links::Model, AccessError> {
        self.covering_vault_link_on(self.db.as_ref(), vault_id, sandbox_id, project_id)
            .await
    }

    pub(super) async fn covering_vault_link_on<C: ConnectionTrait>(
        &self,
        db: &C,
        vault_id: &str,
        sandbox_id: i32,
        project_id: Option<i32>,
    ) -> Result<links::Model, AccessError> {
        let mut coverage = Condition::any().add(links::Column::SandboxId.eq(sandbox_id));
        if let Some(id) = project_id {
            coverage = coverage.add(links::Column::ProjectId.eq(id));
        }
        links::Entity::find()
            .filter(links::Column::VaultId.eq(vault_id))
            .filter(links::Column::RevokedAt.is_null())
            .filter(coverage)
            .order_by_desc(Expr::col(links::Column::SandboxId).is_not_null())
            .order_by_asc(links::Column::Id)
            .one(db)
            .await
            .map_err(|e| AccessError::resource_database("resolve vault link", vault_id, e))?
            .ok_or_else(|| AccessError::UncoveredSandbox {
                vault_id: vault_id.into(),
                sandbox_id,
            })
    }

    /// Management is restricted to instance administrators by every caller.
    /// The same vault lock is held by approvals and credential edits.
    pub async fn link_vault(
        &self,
        vault_id: &str,
        project_id: Option<i32>,
        sandbox_id: Option<i32>,
    ) -> Result<links::Model, AccessError> {
        if project_id.is_some() == sandbox_id.is_some() {
            return Err(invalid("consumer", "select exactly one project or sandbox"));
        }
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::resource_database("begin vault link", vault_id, e))?;
        self.lock_vault_management(&tx).await?;
        let vault = self.vault(&tx, None, vault_id, true).await?;
        let target_project = if let Some(id) = sandbox_id {
            sandboxes::Entity::find_by_id(id)
                .one(&tx)
                .await
                .map_err(|e| AccessError::resource_database("read linked sandbox", vault_id, e))?
                .ok_or_else(|| not_found("sandbox", id.to_string(), project_id))?
                .project_id
        } else {
            self.project(&tx, project_id).await?;
            project_id
        };
        let services = self.selected_link_services(&tx, &vault.service_ids).await?;
        for service in services {
            let policy: SandboxAccessPolicy =
                serde_json::from_value(service.policy).map_err(|source| {
                    AccessError::StoredPolicy {
                        id: service.id.clone(),
                        project_id: service.project_id,
                        source,
                    }
                })?;
            if policy.github_repository.is_some()
                && (service.project_id.is_none() || service.project_id != target_project)
            {
                return Err(AccessError::GitLinkConflict {
                    vault_id: vault_id.into(),
                    service_id: service.id,
                    source_project_id: service.project_id,
                    target_project_id: target_project,
                });
            }
        }
        let mut existing = links::Entity::find()
            .filter(links::Column::VaultId.eq(vault_id))
            .filter(links::Column::RevokedAt.is_null());
        if let Some(id) = sandbox_id {
            existing = existing.filter(links::Column::SandboxId.eq(id));
        }
        if let Some(id) = project_id {
            existing = existing.filter(links::Column::ProjectId.eq(id));
        }
        if let Some(row) = existing
            .one(&tx)
            .await
            .map_err(|e| AccessError::resource_database("read existing vault link", vault_id, e))?
        {
            return Ok(row);
        }
        let row = links::ActiveModel {
            id: Set(format!("link_{}", uuid::Uuid::new_v4().simple())),
            vault_id: Set(vault_id.into()),
            project_id: Set(project_id),
            sandbox_id: Set(sandbox_id),
            created_at: Set(Utc::now()),
            revoked_at: Set(None),
        }
        .insert(&tx)
        .await
        .map_err(|e| AccessError::resource_database("create vault link", vault_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::resource_database("commit vault link", vault_id, e))?;
        Ok(row)
    }

    async fn selected_link_services<C: ConnectionTrait>(
        &self,
        db: &C,
        ids: &[String],
    ) -> Result<Vec<services::Model>, AccessError> {
        services::Entity::find()
            .filter(services::Column::Id.is_in(ids.to_vec()))
            .order_by_asc(services::Column::Id)
            .all(db)
            .await
            .map_err(|e| AccessError::database("read linked vault credential sources", None, e))
    }

    pub async fn unlink_vault(&self, vault_id: &str, link_id: &str) -> Result<(), AccessError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::resource_database("begin vault unlink", vault_id, e))?;
        self.vault(&tx, None, vault_id, true).await?;
        let row = links::Entity::find_by_id(link_id)
            .filter(links::Column::VaultId.eq(vault_id))
            .one(&tx)
            .await
            .map_err(|e| {
                AccessError::resource_database("read vault link for revocation", vault_id, e)
            })?
            .ok_or_else(|| not_found("vault link", link_id.into(), None))?;
        if row.revoked_at.is_none() {
            // The database trigger revokes dependent sessions and their grants;
            // existing grant triggers durably wake worker reconciliation.
            links::Entity::update_many()
                .col_expr(links::Column::RevokedAt, Expr::value(Utc::now()))
                .filter(links::Column::Id.eq(link_id))
                .exec(&tx)
                .await
                .map_err(|e| {
                    AccessError::database(
                        "revoke vault link and dependent access",
                        row.project_id,
                        e,
                    )
                })?;
        }
        tx.commit()
            .await
            .map_err(|e| AccessError::resource_database("commit vault unlink", vault_id, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{broker::UnavailableTokenlessBroker, vault_types::SaveAccessVault};

    #[tokio::test]
    async fn links_cover_projectless_consumers_and_revoke_session_grants() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault links: Docker unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("INSERT INTO users(id,name,email) VALUES(9911,'Fixture','fixture@example.com'); INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9911,'Fixture','repo','owner','/','main','nodejs','vault-link-fixture',now(),now()); INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9911,'sbx_link_fixture','Fixture',9911,NULL,'running','docker'),(9912,'sbx_project_fixture','Project fixture',9911,9911,'running','docker')").await.unwrap();
        let service = ServiceAccessService::new(
            db.clone(),
            super::super::tests::encryption(),
            Arc::new(UnavailableTokenlessBroker),
        );
        let vault = service
            .save_vault(
                None,
                None,
                SaveAccessVault {
                    name: "General".into(),
                    service_ids: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(vault.project_id, None);
        assert!(matches!(
            service.covering_vault_link(&vault.id, 9911, None).await,
            Err(AccessError::UncoveredSandbox {
                sandbox_id: 9911,
                ..
            })
        ));
        let link = service
            .link_vault(&vault.id, None, Some(9911))
            .await
            .unwrap();
        assert_eq!(
            service
                .session_eligibility(&vault.id, "sbx_link_fixture", 9911, false)
                .await
                .unwrap()
                .id,
            link.id
        );
        assert!(service
            .session_eligibility(&vault.id, "sbx_link_fixture", 9912, false)
            .await
            .is_err());
        let project_link = service
            .link_vault(&vault.id, Some(9911), None)
            .await
            .unwrap();
        assert_eq!(
            service
                .covering_vault_link(&vault.id, 9912, Some(9911))
                .await
                .unwrap()
                .id,
            project_link.id
        );
        assert!(service.link_vault(&vault.id, None, None).await.is_err());
        assert!(
            matches!(service.link_vault(&vault.id,None,Some(9999)).await,Err(AccessError::NotFound { id,.. }) if id=="9999")
        );
        let values = [vault.id.clone().into(), link.id.clone().into()];
        db.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO sandbox_access_services(id,name,policy,max_ttl_seconds,enabled) VALUES('svc_fixture','Fixture','{}',60,true)",[])).await.unwrap();
        db.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO access_bundles(id,vault_id,name,service_ids,ttl_seconds) VALUES('bundle_fixture',$1,'Fixture',ARRAY['svc_fixture'],60)",[vault.id.clone().into()])).await.unwrap();
        db.execute_unprepared("INSERT INTO sandbox_service_grants(id,service_id,sandbox_id,service_version,policy_snapshot,created_by,expires_at) VALUES('grant_fixture','svc_fixture',9911,1,'{}',9911,now()+interval '60 seconds')").await.unwrap();
        db.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO access_sessions(id,vault_id,link_id,bundle_id,bundle_name,bundle_version,consumer_kind,consumer_id,sandbox_id,created_by,consent,grant_ids,created_at,expires_at) VALUES('session_fixture',$1,$2,'bundle_fixture','Fixture',1,'sandbox','sbx_link_fixture',9911,9911,'{}',ARRAY['grant_fixture'],now(),now()+interval '60 seconds')",values)).await.unwrap();
        db.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE access_vaults SET service_ids=ARRAY['svc_fixture'] WHERE id=$1",
            [vault.id.clone().into()],
        ))
        .await
        .unwrap();
        assert!(
            matches!(service.require_standalone_service(db.as_ref(), "svc_fixture").await,
            Err(AccessError::VaultSessionRequired { service_id, vault_id }) if service_id == "svc_fixture" && vault_id == vault.id)
        );
        service.unlink_vault(&vault.id, &link.id).await.unwrap();
        let grant = grants::Entity::find_by_id("grant_fixture")
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(grant.revoked_at.is_some());
        assert_eq!(grant.version, 2);
        let session = temps_entities::access_sessions::Entity::find_by_id("session_fixture")
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert!(session.revoked_at.is_some());
        assert!(service
            .covering_vault_link(&vault.id, 9911, None)
            .await
            .is_err());
        service
            .retire_vault_resource(&vault.id, None)
            .await
            .unwrap();
        assert!(service.get_vault(None, &vault.id).await.is_err());
        // Connection-backed credentials cannot widen their source project's audience.
        let git_vault = service
            .save_vault(
                None,
                None,
                SaveAccessVault {
                    name: "Source-bound".into(),
                    service_ids: vec![],
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let policy = SandboxAccessPolicy {
            provider: None,
            origin: "https://api.github.com".into(),
            path_prefixes: vec!["/repos/example/repository".into()],
            methods: vec![AccessMethod::Get],
            github_repository: Some(GithubRepositoryAccess {
                connection_id: 1,
                owner: "example".into(),
                repository: "repository".into(),
                protocol: GithubAccessProtocol::Api,
            }),
        };
        db.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO sandbox_access_services(id,project_id,name,policy,max_ttl_seconds,enabled) VALUES('svc_git_fixture',9911,'Git fixture',$1,60,true)",[serde_json::to_value(policy).unwrap().into()])).await.unwrap();
        db.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE access_vaults SET service_ids=ARRAY['svc_git_fixture'] WHERE id=$1",
            [git_vault.id.clone().into()],
        ))
        .await
        .unwrap();
        assert!(matches!(
            service.link_vault(&git_vault.id, None, Some(9911)).await,
            Err(AccessError::GitLinkConflict {
                source_project_id: Some(9911),
                target_project_id: None,
                ..
            })
        ));
        service
            .link_vault(&git_vault.id, Some(9911), None)
            .await
            .unwrap();
        let named = service
            .list_vault_links(
                &git_vault.id,
                AccessPageQuery {
                    page: None,
                    page_size: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(named.items[0].consumer_name.as_deref(), Some("Fixture"));
        service.retire_service("svc_git_fixture").await.unwrap();
        assert!(
            matches!(service.get_service(None,"svc_git_fixture").await,Err(AccessError::NotFound {id,..}) if id=="svc_git_fixture")
        );
        assert!(service
            .get_vault(None, &git_vault.id)
            .await
            .unwrap()
            .services
            .is_empty());
        test.cleanup().await;
    }
}
