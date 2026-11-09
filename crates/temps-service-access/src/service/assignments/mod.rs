// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Durable approvals are separate from expiring, incarnation-bound runtime grants.
mod renewal;
use super::*;
use crate::assignment_types::*;
use sea_orm::sea_query::Expr;
use temps_entities::sandbox_service_assignments as assignments;

impl ServiceAccessService {
    pub async fn assignment_service(
        &self,
        project_id: i32,
        sandbox_id: &str,
        id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<AccessServiceResponse, AccessError> {
        let sandbox = self
            .sandbox(self.db.as_ref(), project_id, sandbox_id, user_id, admin)
            .await?;
        let row = assignments::Entity::find_by_id(id)
            .filter(assignments::Column::ProjectId.eq(project_id))
            .filter(assignments::Column::SandboxId.eq(sandbox.id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| AccessError::database("read scoped approval service", project_id, e))?
            .ok_or_else(|| not_found("service approval", id.into(), project_id))?;
        self.get_service(project_id, &row.service_id).await
    }

    pub async fn request_sandbox_service(
        &self,
        project_id: i32,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
        request: RequestSandboxService,
    ) -> Result<SandboxServiceResponse, AccessError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin service declaration", project_id, e))?;
        self.project(&tx, project_id).await?;
        let service = services::Entity::find_by_id(&request.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read declared service", project_id, e))?
            .ok_or_else(|| not_found("service", request.service_id.clone(), project_id))?;
        if service.version != request.service_version {
            return Err(AccessError::Conflict {
                id: service.id,
                version: request.service_version,
            });
        }
        let sandbox = self
            .sandbox(&tx, project_id, sandbox_id, user_id, admin)
            .await?;
        // Serialize duplicate requests and the per-sandbox capacity check.
        let _ = sandboxes::Entity::find_by_id(sandbox.id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock declared sandbox", project_id, e))?;
        if sandbox.status == "destroyed" {
            return Err(invalid(
                "sandbox_id",
                "destroyed sandboxes cannot request access",
            ));
        }
        let existing = assignments::Entity::find()
            .filter(assignments::Column::SandboxId.eq(sandbox.id))
            .filter(assignments::Column::ServiceId.eq(&service.id))
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read existing declaration", project_id, e))?;
        let row = if let Some(row) = existing {
            row
        } else {
            if assignments::Entity::find()
                .filter(assignments::Column::SandboxId.eq(sandbox.id))
                .filter(assignments::Column::RevokedAt.is_null())
                .filter(
                    sea_orm::Condition::any()
                        .add(assignments::Column::ExpiresAt.is_null())
                        .add(assignments::Column::ExpiresAt.gt(Utc::now())),
                )
                .count(&tx)
                .await
                .map_err(|e| AccessError::database("count sandbox declarations", project_id, e))?
                >= 16
            {
                return Err(invalid(
                    "services",
                    "a sandbox can declare at most 16 services",
                ));
            }
            assignments::ActiveModel {
                id: Set(format!("access_{}", uuid::Uuid::new_v4().simple())),
                project_id: Set(project_id),
                sandbox_id: Set(sandbox.id),
                service_id: Set(service.id.clone()),
                service_version: Set(service.version),
                requested_by: Set(Some(user_id)),
                approved_by: Set(None),
                approved_at: Set(None),
                expires_at: Set(None),
                revoked_at: Set(None),
                version: Set(1),
                grant_id: Set(None),
                next_reconcile_at: Set(Some(Utc::now())),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
            }
            .insert(&tx)
            .await
            .map_err(|e| AccessError::database("save service declaration", project_id, e))?
        };
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit service declaration", project_id, e))?;
        assignment_response(row, &service, &sandbox)
    }

    pub async fn list_sandbox_services(
        &self,
        project_id: i32,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
        query: AccessPageQuery,
    ) -> Result<SandboxServiceList, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let sandbox = self
            .sandbox(self.db.as_ref(), project_id, sandbox_id, user_id, admin)
            .await?;
        let (page, page_size) = query.normalized();
        let pages = assignments::Entity::find()
            .filter(assignments::Column::ProjectId.eq(project_id))
            .filter(assignments::Column::SandboxId.eq(sandbox.id))
            .order_by_desc(assignments::Column::CreatedAt)
            .order_by_desc(assignments::Column::Id)
            .find_also_related(services::Entity)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count service access", project_id, e))?;
        let rows = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list service access", project_id, e))?;
        let grant_ids: Vec<_> = rows
            .iter()
            .filter_map(|(row, _)| row.grant_id.clone())
            .collect();
        let grant_rows = grants::Entity::find()
            .filter(grants::Column::ProjectId.eq(project_id))
            .filter(grants::Column::SandboxId.eq(sandbox.id))
            .filter(grants::Column::Id.is_in(grant_ids))
            .find_also_related(services::Entity)
            .all(self.db.as_ref())
            .await
            .map_err(|e| AccessError::database("read access enforcement grants", project_id, e))?;
        let mut receipts = grant_rows
            .into_iter()
            .map(|(g, s)| {
                let s = s.ok_or_else(|| not_found("service", g.service_id.clone(), project_id))?;
                grant_response(g, &s, sandbox_id, Utc::now())
            })
            .collect::<Result<Vec<_>, AccessError>>()?;
        self.observe_grants(project_id, sandbox.id, &mut receipts)
            .await?;
        let items = rows
            .into_iter()
            .map(|(row, service)| {
                let grant_id = row.grant_id.clone();
                let service = service
                    .ok_or_else(|| not_found("service", row.service_id.clone(), project_id))?;
                let mut item = assignment_response(row, &service, &sandbox)?;
                if let Some(receipt) = receipts.iter().find(|g| Some(&g.id) == grant_id.as_ref()) {
                    item.enforcement = receipt.enforcement.clone();
                    if item.status == SandboxServiceStatus::Approved {
                        item.connected = receipt.connected;
                        item.reason = if receipt.connected { "Access is active. Temps renews internal leases while this approval remains valid.".into() } else { receipt.reason.clone() };
                    }
                }
                Ok(item)
            })
            .collect::<Result<_, AccessError>>()?;
        Ok(SandboxServiceList {
            items,
            page,
            page_size,
            total,
        })
    }

    pub async fn approve_sandbox_service(
        &self,
        project_id: i32,
        sandbox_id: &str,
        id: &str,
        user_id: i32,
        admin: bool,
        request: ApproveSandboxService,
    ) -> Result<SandboxServiceResponse, AccessError> {
        if request
            .expires_at
            .is_some_and(|expiry| expiry <= Utc::now() + Duration::seconds(60))
        {
            return Err(invalid(
                "expires_at",
                "fixed approval must expire more than 60 seconds from now",
            ));
        }
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin durable approval", project_id, e))?;
        self.project(&tx, project_id).await?;
        let sandbox = self
            .sandbox(&tx, project_id, sandbox_id, user_id, admin)
            .await?;
        if sandbox.status == "destroyed" {
            return Err(invalid(
                "sandbox_id",
                "destroyed sandboxes cannot be approved",
            ));
        }
        let row = self.assignment(&tx, project_id, sandbox.id, id).await?;
        if row.version != request.expected_version || row.version == i32::MAX {
            return Err(AccessError::Conflict {
                id: id.into(),
                version: request.expected_version,
            });
        }
        let service = services::Entity::find_by_id(&row.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read approved policy", project_id, e))?
            .ok_or_else(|| not_found("service", row.service_id.clone(), project_id))?;
        if service.version != request.service_version {
            return Err(AccessError::Conflict {
                id: service.id,
                version: request.service_version,
            });
        }
        if !service.enabled
            || (service.credential_encrypted.is_none()
                && service.policy.get("github_repository").is_none())
        {
            return Err(invalid(
                "service_id",
                "enable the service and configure its credential before approval",
            ));
        }
        // Reapproving a historical declaration consumes capacity again. The
        // sandbox lock serializes this with new requests and other approvals.
        let _ = sandboxes::Entity::find_by_id(sandbox.id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock approval capacity", project_id, e))?;
        let active = assignments::Entity::find()
            .filter(assignments::Column::SandboxId.eq(sandbox.id))
            .filter(assignments::Column::Id.ne(id))
            .filter(assignments::Column::RevokedAt.is_null())
            .filter(
                sea_orm::Condition::any()
                    .add(assignments::Column::ExpiresAt.is_null())
                    .add(assignments::Column::ExpiresAt.gt(Utc::now())),
            )
            .count(&tx)
            .await
            .map_err(|e| AccessError::database("count approval capacity", project_id, e))?;
        if active >= 16 {
            return Err(invalid(
                "services",
                "a sandbox can declare at most 16 active services",
            ));
        }
        cut_assignment_grants(&tx, project_id, id).await?;
        let mut model = row.into_active_model();
        model.approved_by = Set(Some(user_id));
        model.approved_at = Set(Some(Utc::now()));
        model.expires_at = Set(request.expires_at);
        model.revoked_at = Set(None);
        model.service_version = Set(service.version);
        model.version = Set(request.expected_version + 1);
        model.grant_id = Set(None);
        model.next_reconcile_at = Set(Some(Utc::now()));
        model.updated_at = Set(Utc::now());
        let row = model
            .update(&tx)
            .await
            .map_err(|e| AccessError::database("save durable approval", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit durable approval", project_id, e))?;
        assignment_response(row, &service, &sandbox)
    }

    pub async fn revoke_sandbox_service(
        &self,
        project_id: i32,
        sandbox_id: &str,
        id: &str,
        user_id: i32,
        admin: bool,
        version: i32,
    ) -> Result<SandboxServiceResponse, AccessError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin durable revocation", project_id, e))?;
        let sandbox = self
            .sandbox(&tx, project_id, sandbox_id, user_id, admin)
            .await?;
        let row = self.assignment(&tx, project_id, sandbox.id, id).await?;
        if row.version != version || version == i32::MAX {
            return Err(AccessError::Conflict {
                id: id.into(),
                version,
            });
        }
        let service = services::Entity::find_by_id(&row.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("read revoked service", project_id, e))?
            .ok_or_else(|| not_found("service", row.service_id.clone(), project_id))?;
        cut_assignment_grants(&tx, project_id, id).await?;
        let mut model = row.into_active_model();
        model.revoked_at = Set(Some(Utc::now()));
        model.version = Set(version + 1);
        model.updated_at = Set(Utc::now());
        let row = model
            .update(&tx)
            .await
            .map_err(|e| AccessError::database("save durable revocation", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit durable revocation", project_id, e))?;
        assignment_response(row, &service, &sandbox)
    }

    async fn assignment<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
        sandbox_id: i32,
        id: &str,
    ) -> Result<assignments::Model, AccessError> {
        assignments::Entity::find_by_id(id)
            .filter(assignments::Column::ProjectId.eq(project_id))
            .filter(assignments::Column::SandboxId.eq(sandbox_id))
            .lock_exclusive()
            .one(db)
            .await
            .map_err(|e| AccessError::database("lock scoped service approval", project_id, e))?
            .ok_or_else(|| not_found("service approval", id.into(), project_id))
    }
}
async fn cut_assignment_grants<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    id: &str,
) -> Result<(), AccessError> {
    grants::Entity::update_many()
        .col_expr(grants::Column::RevokedAt, Expr::value(Utc::now()))
        .col_expr(
            grants::Column::Version,
            Expr::col(grants::Column::Version).add(1),
        )
        .filter(grants::Column::AssignmentId.eq(id))
        .filter(grants::Column::ProjectId.eq(project_id))
        .filter(grants::Column::RevokedAt.is_null())
        .exec(db)
        .await
        .map_err(|e| AccessError::database("cut durable service grants", project_id, e))?;
    Ok(())
}
fn assignment_response(
    row: assignments::Model,
    service: &services::Model,
    sandbox: &sandboxes::Model,
) -> Result<SandboxServiceResponse, AccessError> {
    let status = if row.revoked_at.is_some() {
        SandboxServiceStatus::Revoked
    } else if row.expires_at.is_some_and(|e| e <= Utc::now()) {
        SandboxServiceStatus::Expired
    } else if service.version != row.service_version {
        SandboxServiceStatus::PolicyChanged
    } else if !service.enabled {
        SandboxServiceStatus::ServiceDisabled
    } else if row.approved_at.is_none() {
        SandboxServiceStatus::PendingApproval
    } else if sandbox.status != "running" {
        SandboxServiceStatus::Suspended
    } else {
        SandboxServiceStatus::Approved
    };
    let reason=match status {
        SandboxServiceStatus::PendingApproval=>"This service needs operator approval.",
        SandboxServiceStatus::Approved=>"Approved. Waiting for the worker to confirm access; internal leases renew automatically.",
        SandboxServiceStatus::Suspended=>"Access is suspended while the sandbox is stopped. Resume rechecks the approval.",
        SandboxServiceStatus::Revoked=>"Access was revoked. Requests cannot restore it without a new approval.",
        SandboxServiceStatus::Expired=>"The fixed approval deadline has expired.",
        SandboxServiceStatus::PolicyChanged=>"The service policy changed. Review and approve the new scope.",
        SandboxServiceStatus::ServiceDisabled=>"The service is disabled.",
    };
    Ok(SandboxServiceResponse {
        id: row.id,
        service_id: row.service_id,
        service_name: service.name.clone(),
        service_version: service.version,
        policy: serde_json::from_value(service.policy.clone()).map_err(|source| {
            AccessError::StoredPolicy {
                id: service.id.clone(),
                project_id: row.project_id,
                source,
            }
        })?,
        version: row.version,
        status,
        enforcement: GrantEnforcement::NotRequested,
        connected: false,
        reason: reason.into(),
        expires_at: row.expires_at,
        created_at: row.created_at,
    })
}

#[cfg(test)]
mod tests;
