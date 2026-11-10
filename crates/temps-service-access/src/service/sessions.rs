// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vault sessions are immutable approvals. Their grants and session record commit
//! together; expiry and revocation use the existing worker enforcement path.
use super::*;
use crate::session_types::*;
use sea_orm::{sea_query::Expr, DatabaseBackend, JoinType, RelationTrait, Statement};
use std::collections::BTreeSet;
use temps_entities::{access_bundles as bundles, access_sessions as sessions};
use uuid::Uuid;

pub(crate) fn validate(request: &CreateAccessSession) -> Result<(), AccessError> {
    if request.request_id.is_nil() || !(60..=86400).contains(&request.ttl_seconds) {
        return Err(invalid(
            "session",
            "a request UUID and lifetime between 60 and 86400 seconds are required",
        ));
    }
    if request.bundle_id.is_empty()
        || request.bundle_id.len() > 128
        || request.bundle_version <= 0
        || request.consumer.sandbox_id().is_empty()
        || request.consumer.sandbox_id().len() > 128
    {
        return Err(invalid(
            "consumer",
            "a bundle version and specific sandbox are required",
        ));
    }
    if request.services.is_empty()
        || request.services.len() > 16
        || request
            .services
            .iter()
            .any(|s| s.version <= 0 || s.service_id.is_empty() || s.service_id.len() > 128)
        || request
            .services
            .iter()
            .map(|s| &s.service_id)
            .collect::<BTreeSet<_>>()
            .len()
            != request.services.len()
    {
        return Err(invalid(
            "services",
            "review between 1 and 16 distinct service policies",
        ));
    }
    Ok(())
}

fn response(row: sessions::Model, now: chrono::DateTime<Utc>) -> AccessSessionResponse {
    let status = if row.revoked_at.is_some() {
        AccessSessionStatus::Revoked
    } else if row.expires_at <= now {
        AccessSessionStatus::Expired
    } else {
        AccessSessionStatus::Issued
    };
    AccessSessionResponse {
        id: row.id,
        project_id: row.project_id,
        vault_id: row.vault_id,
        bundle_id: row.bundle_id,
        bundle_name: row.bundle_name,
        bundle_version: row.bundle_version,
        consumer: AccessSessionConsumer::Sandbox {
            sandbox_id: row.consumer_id,
        },
        created_by: row.created_by,
        status,
        service_count: row.grant_ids.len(),
        created_at: row.created_at,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
    }
}

impl ServiceAccessService {
    /// The handler checks both project/sandbox permissions and Git permission
    /// when selected policies use a project GitHub connection.
    pub async fn authorize_session(
        &self,
        project_id: impl Into<Option<i32>>,
        vault_id: &str,
        user_id: i32,
        admin: bool,
        request: CreateAccessSession,
        observed: HostObservation,
    ) -> Result<AccessSessionResponse, AccessError> {
        let project_id = project_id.into();
        validate(&request)?;
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin vault session", project_id, e))?;
        self.project(&tx, project_id).await?;
        // Same lock as bundle writers serializes policy selection and retries.
        self.vault(&tx, None, vault_id, true).await?;
        let sandbox = self
            .sandbox(
                &tx,
                project_id,
                request.consumer.sandbox_id(),
                user_id,
                admin,
            )
            .await?;
        let link = self
            .covering_vault_link_on(&tx, vault_id, sandbox.id, sandbox.project_id)
            .await?;
        if request
            .authorized_link_id
            .as_ref()
            .is_some_and(|id| id != &link.id)
        {
            return Err(AccessError::Conflict {
                id: vault_id.into(),
                version: 0,
            });
        }
        let project_id = sandbox.project_id;
        let id = format!("session_{}", request.request_id.simple());
        let consent =
            serde_json::to_value(&request).map_err(|source| AccessError::StoredPolicy {
                id: id.clone(),
                project_id,
                source,
            })?;
        if let Some(existing) = sessions::Entity::find_by_id(&id)
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read session retry", project_id, e))?
        {
            if existing.project_id != project_id
                || existing.vault_id != vault_id
                || existing.sandbox_id != sandbox.id
                || existing.created_by != Some(user_id)
                || existing.consent != consent
            {
                return Err(invalid(
                    "request_id",
                    "already used for different session consent",
                ));
            }
            // Returning the original record never extends it or reinstalls access.
            return Ok(response(existing, Utc::now()));
        }
        let bundle = bundles::Entity::find_by_id(&request.bundle_id)
            .filter(bundles::Column::RetiredAt.is_null())
            .filter(bundles::Column::VaultId.eq(vault_id))
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read session bundle", project_id, e))?
            .ok_or_else(|| not_found("bundle", request.bundle_id.clone(), project_id))?;
        if bundle.version != request.bundle_version {
            return Err(AccessError::Conflict {
                id: bundle.id,
                version: request.bundle_version,
            });
        }
        if request.ttl_seconds > bundle.ttl_seconds {
            return Err(invalid(
                "ttl_seconds",
                "exceeds the access bundle maximum lifetime",
            ));
        }
        let ids: BTreeSet<_> = request
            .services
            .iter()
            .map(|s| s.service_id.clone())
            .collect();
        if ids != bundle.service_ids.iter().cloned().collect() {
            return Err(invalid(
                "services",
                "review the exact current service selection in the bundle",
            ));
        }
        // Lock every service before taking the sandbox lock in grant issuance.
        // This preserves service -> sandbox ordering across single/batch approvals.
        let locked = services::Entity::find()
            .filter(services::Column::Id.is_in(ids))
            .order_by_asc(services::Column::Id)
            .lock_exclusive()
            .all(&tx)
            .await
            .map_err(|e| AccessError::database("lock session services", project_id, e))?;
        if locked.len() != request.services.len() {
            return Err(invalid(
                "services",
                "a selected service is no longer in this project",
            ));
        }
        let now = Utc::now();
        let versions: std::collections::BTreeMap<_, _> = request
            .services
            .iter()
            .map(|s| (s.service_id.as_str(), s.version))
            .collect();
        let selections = locked
            .into_iter()
            .map(|service| {
                let version = versions
                    .get(service.id.as_str())
                    .copied()
                    .ok_or_else(|| invalid("services", "locked service was not selected"))?;
                Ok((
                    Uuid::new_v4(),
                    RequestServiceGrant {
                        service_id: service.id.clone(),
                        service_version: version,
                        ttl_seconds: request.ttl_seconds,
                    },
                    service,
                ))
            })
            .collect::<Result<Vec<_>, AccessError>>()?;
        let grant_ids = self
            .authorize_locked_grants(
                &tx,
                super::consent::LockedConsentBatch {
                    project_id,
                    sandbox_id: sandbox.public_id.clone(),
                    user_id,
                    admin,
                    selections,
                },
                observed,
                now,
            )
            .await?
            .into_iter()
            .map(|grant| grant.grant_id)
            .collect();
        let row = sessions::ActiveModel {
            id: Set(id),
            project_id: Set(project_id),
            vault_id: Set(vault_id.into()),
            link_id: Set(Some(link.id)),
            bundle_id: Set(bundle.id),
            bundle_name: Set(bundle.name),
            bundle_version: Set(bundle.version),
            consumer_kind: Set("sandbox".into()),
            consumer_id: Set(sandbox.public_id),
            sandbox_id: Set(sandbox.id),
            created_by: Set(Some(user_id)),
            consent: Set(consent),
            grant_ids: Set(grant_ids),
            created_at: Set(now),
            expires_at: Set(now + Duration::seconds(i64::from(request.ttl_seconds))),
            revoked_at: Set(None),
        }
        .insert(&tx)
        .await
        .map_err(|e| AccessError::database("persist vault session", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit vault session", project_id, e))?;
        Ok(response(row, Utc::now()))
    }

    fn visible_sessions(
        project_id: Option<i32>,
        user_id: i32,
        admin: bool,
    ) -> sea_orm::Select<sessions::Entity> {
        let query = sessions::Entity::find()
            .join(JoinType::InnerJoin, sessions::Relation::Sandbox.def())
            .apply_if(project_id, |q, id| {
                q.filter(sessions::Column::ProjectId.eq(id))
            })
            .apply_if(project_id, |q, id| {
                q.filter(sandboxes::Column::ProjectId.eq(id))
            });
        if admin {
            query
        } else {
            query.filter(sandboxes::Column::UserId.eq(user_id))
        }
    }

    pub async fn list_sessions(
        &self,
        project_id: impl Into<Option<i32>>,
        user_id: i32,
        admin: bool,
        query: AccessSessionQuery,
    ) -> Result<AccessSessionList, AccessError> {
        self.list_sessions_filtered(project_id.into(), user_id, admin, query, false)
            .await
    }

    /// A non-admin global listing is bounded to one owned sandbox. When its
    /// project permissions are absent, only direct-link sessions are visible.
    pub async fn list_sessions_filtered(
        &self,
        project_id: Option<i32>,
        user_id: i32,
        admin: bool,
        query: AccessSessionQuery,
        direct_only: bool,
    ) -> Result<AccessSessionList, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let (page, page_size) = AccessPageQuery {
            page: query.page,
            page_size: query.page_size,
        }
        .normalized();
        let mut selection = Self::visible_sessions(project_id, user_id, admin);
        if direct_only {
            selection = selection.filter(Expr::cust("EXISTS (SELECT 1 FROM access_vault_links l WHERE l.id=access_sessions.link_id AND l.sandbox_id=access_sessions.sandbox_id)"));
        }
        if let Some(vault_id) = query.vault_id {
            self.vault(self.db.as_ref(), project_id, &vault_id, false)
                .await?;
            selection = selection.filter(sessions::Column::VaultId.eq(vault_id));
        }
        if let Some(sandbox_id) = query.sandbox_id {
            self.sandbox(self.db.as_ref(), project_id, &sandbox_id, user_id, admin)
                .await?;
            selection = selection.filter(sessions::Column::ConsumerId.eq(sandbox_id));
        }
        let pages = selection
            .order_by_desc(sessions::Column::CreatedAt)
            .order_by_desc(sessions::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count vault sessions", project_id, e))?;
        let now = Utc::now();
        let items = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list vault sessions", project_id, e))?
            .into_iter()
            .map(|row| response(row, now))
            .collect();
        Ok(AccessSessionList {
            items,
            total,
            page,
            page_size,
        })
    }

    async fn session<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: impl Into<Option<i32>>,
        id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<sessions::Model, AccessError> {
        let project_id = project_id.into();
        Self::visible_sessions(project_id, user_id, admin)
            .filter(sessions::Column::Id.eq(id))
            .one(db)
            .await
            .map_err(|e| AccessError::database("read scoped vault session", project_id, e))?
            .ok_or_else(|| not_found("session", id.into(), project_id))
    }

    /// Resolve the immutable authorization context, including revoked links.
    /// Missing/deleted provenance falls back to the historical project scope.
    pub async fn session_project_scope(
        &self,
        id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<Option<i32>, AccessError> {
        let row = self.db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT CASE WHEN l.sandbox_id IS NOT NULL THEN NULL ELSE s.project_id END AS project_id FROM access_sessions s JOIN sandboxes b ON b.id=s.sandbox_id LEFT JOIN access_vault_links l ON l.id=s.link_id WHERE s.id=$1 AND ($2 OR b.user_id=$3)",
            [id.into(), admin.into(), user_id.into()],
        )).await.map_err(|source| AccessError::ResourceDatabase { operation: "resolve session project scope", resource_id: id.into(), source })?
          .ok_or_else(|| not_found("session", id.into(), None))?;
        row.try_get("", "project_id")
            .map_err(|source| AccessError::ResourceDatabase {
                operation: "decode session project scope",
                resource_id: id.into(),
                source,
            })
    }

    pub async fn get_session(
        &self,
        project_id: impl Into<Option<i32>>,
        id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<AccessSessionDetail, AccessError> {
        let project_id = project_id.into();
        self.project(self.db.as_ref(), project_id).await?;
        let row = self
            .session(self.db.as_ref(), project_id, id, user_id, admin)
            .await?;
        let now = Utc::now();
        let mut grants = grants::Entity::find()
            .filter(grants::Column::SandboxId.eq(row.sandbox_id))
            .filter(grants::Column::Id.is_in(row.grant_ids.clone()))
            .order_by_asc(grants::Column::Id)
            .find_also_related(services::Entity)
            .all(self.db.as_ref())
            .await
            .map_err(|e| AccessError::database("read session grant policies", project_id, e))?
            .into_iter()
            .map(|(grant, service)| {
                let service = service
                    .ok_or_else(|| not_found("service", grant.service_id.clone(), project_id))?;
                grant_response(grant, &service, &row.consumer_id, now)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.observe_grants(row.project_id, row.sandbox_id, &mut grants)
            .await?;
        Ok(AccessSessionDetail {
            session: response(row, now),
            grants,
        })
    }

    pub async fn revoke_session(
        &self,
        project_id: impl Into<Option<i32>>,
        id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<AccessSessionResponse, AccessError> {
        let project_id = project_id.into();
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin session revocation", project_id, e))?;
        self.project(&tx, project_id).await?;
        let visible = self.session(&tx, project_id, id, user_id, admin).await?;
        if visible.revoked_at.is_some() {
            return Ok(response(visible, Utc::now()));
        }
        // Serialize with unlink/retirement, then update grants before the session,
        // matching the revocation trigger's lock order.
        self.vault(&tx, None, &visible.vault_id, true).await?;
        let row = sessions::Entity::find_by_id(&visible.id)
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock session revocation", project_id, e))?
            .ok_or_else(|| not_found("session", id.into(), project_id))?;
        if row.revoked_at.is_some() {
            return Ok(response(row, Utc::now()));
        }
        // One bounded update. Existing grant triggers durably wake worker removal;
        // an HTTP success still does not claim the worker has acknowledged it.
        grants::Entity::update_many()
            .col_expr(grants::Column::RevokedAt, Expr::value(Utc::now()))
            .col_expr(
                grants::Column::Version,
                Expr::col(grants::Column::Version).add(1),
            )
            .filter(grants::Column::SandboxId.eq(row.sandbox_id))
            .filter(grants::Column::Id.is_in(row.grant_ids.clone()))
            .filter(grants::Column::RevokedAt.is_null())
            .exec(&tx)
            .await
            .map_err(|e| AccessError::database("revoke session grants", project_id, e))?;
        let mut model = row.into_active_model();
        model.revoked_at = Set(Some(Utc::now()));
        let row = model
            .update(&tx)
            .await
            .map_err(|e| AccessError::database("mark session revoked", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit session revocation", project_id, e))?;
        Ok(response(row, Utc::now()))
    }
}

#[cfg(test)]
mod tests;
