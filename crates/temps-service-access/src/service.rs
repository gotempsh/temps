// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod consent;
pub use consent::{AuthorizedGrant, FreshConsent, HostObservation};

use crate::{
    broker::{AccessCapability, ServiceAccessBroker},
    error::AccessError,
    types::*,
};
use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{
    projects, sandbox_access_services as services, sandbox_service_grants as grants, sandboxes,
};

pub struct ServiceAccessService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    broker: Arc<dyn ServiceAccessBroker>,
}
impl ServiceAccessService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        broker: Arc<dyn ServiceAccessBroker>,
    ) -> Self {
        Self {
            db,
            encryption,
            broker,
        }
    }
    pub fn capability(&self) -> AccessCapability {
        self.broker.capability()
    }

    async fn project<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
    ) -> Result<(), AccessError> {
        if projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::Id)
            .into_tuple::<i32>()
            .one(db)
            .await
            .map_err(|e| AccessError::database("find project", project_id, e))?
            .is_none()
        {
            return Err(not_found("project", project_id.to_string(), project_id));
        }
        Ok(())
    }

    pub async fn get_service(
        &self,
        project_id: i32,
        service_id: &str,
    ) -> Result<AccessServiceResponse, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let model = services::Entity::find_by_id(service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| AccessError::database("read service", project_id, e))?
            .ok_or_else(|| not_found("service", service_id.into(), project_id))?;
        service_response(model)
    }

    pub async fn list_services(
        &self,
        project_id: i32,
        query: AccessPageQuery,
    ) -> Result<AccessServiceList, AccessError> {
        self.project(self.db.as_ref(), project_id).await?;
        let (page, page_size) = query.normalized();
        let pages = services::Entity::find()
            .filter(services::Column::ProjectId.eq(project_id))
            .order_by_desc(services::Column::CreatedAt)
            .order_by_desc(services::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count services", project_id, e))?;
        let items = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list services", project_id, e))?
            .into_iter()
            .map(service_response)
            .collect::<Result<_, _>>()?;
        Ok(AccessServiceList {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn save_service(
        &self,
        project_id: i32,
        id: Option<String>,
        mut request: SaveAccessService,
    ) -> Result<AccessServiceResponse, AccessError> {
        validate_service(&mut request)?;
        if id.is_some() != request.expected_version.is_some() {
            return Err(invalid(
                "expected_version",
                "required for updates and forbidden for creation",
            ));
        }
        // Never log the request or the encryption error's source.
        let encrypted = request
            .credential
            .as_ref()
            .map(|value| {
                self.encryption
                    .encrypt_string(value.expose())
                    .map_err(|source| AccessError::Encryption { project_id, source })
            })
            .transpose()?;
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin service save", project_id, e))?;
        self.project(&tx, project_id).await?;
        if let Some(source) = &request.policy.github_repository {
            crate::github::validate_binding(&tx, project_id, source).await?;
        }
        let now = Utc::now();
        let updating = id.is_some();
        let mut model = if let Some(id) = id {
            let current = services::Entity::find_by_id(&id)
                .filter(services::Column::ProjectId.eq(project_id))
                .lock_exclusive()
                .one(&tx)
                .await
                .map_err(|e| AccessError::database("lock service", project_id, e))?
                .ok_or_else(|| not_found("service", id.clone(), project_id))?;
            let version = request.expected_version.unwrap_or_default();
            if current.version != version || current.version == i32::MAX {
                return Err(AccessError::Conflict { id, version });
            }
            let mut model = current.into_active_model();
            model.version = Set(version + 1);
            model
        } else {
            services::ActiveModel {
                id: Set(format!("svc_{}", uuid::Uuid::new_v4().simple())),
                project_id: Set(project_id),
                created_at: Set(now),
                version: Set(1),
                credential_encrypted: Set(None),
                ..Default::default()
            }
        };
        if request.policy.github_repository.is_some() {
            model.credential_encrypted = Set(None);
        }
        model.name = Set(request.name);
        model.policy = Set(serde_json::to_value(&request.policy).map_err(|source| {
            AccessError::StoredPolicy {
                id: "new policy".into(),
                project_id,
                source,
            }
        })?);
        model.max_ttl_seconds = Set(request.max_ttl_seconds);
        model.enabled = Set(request.enabled);
        model.updated_at = Set(now);
        if let Some(encrypted) = encrypted {
            model.credential_encrypted = Set(Some(encrypted));
        } else if request.clear_credential {
            model.credential_encrypted = Set(None);
        }
        let saved = if updating {
            model.update(&tx).await
        } else {
            model.insert(&tx).await
        }
        .map_err(|e| AccessError::database("save service", project_id, e))?;
        let result = service_response(saved)?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit service save", project_id, e))?;
        Ok(result)
    }

    async fn sandbox<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
    ) -> Result<sandboxes::Model, AccessError> {
        let mut query = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(sandbox_id))
            .filter(sandboxes::Column::ProjectId.eq(project_id));
        if !admin {
            query = query.filter(sandboxes::Column::UserId.eq(user_id));
        }
        query
            .one(db)
            .await
            .map_err(|e| AccessError::database("find sandbox", project_id, e))?
            .ok_or_else(|| not_found("sandbox", sandbox_id.into(), project_id))
    }

    pub async fn list_grants(
        &self,
        project_id: i32,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
        query: AccessPageQuery,
    ) -> Result<ServiceGrantList, AccessError> {
        let sandbox = self
            .sandbox(self.db.as_ref(), project_id, sandbox_id, user_id, admin)
            .await?;
        let (page, page_size) = query.normalized();
        let pages = grants::Entity::find()
            .filter(grants::Column::ProjectId.eq(project_id))
            .filter(grants::Column::SandboxId.eq(sandbox.id))
            .order_by_desc(grants::Column::CreatedAt)
            .order_by_desc(grants::Column::Id)
            .find_also_related(services::Entity)
            .paginate(self.db.as_ref(), page_size);
        let total = pages
            .num_items()
            .await
            .map_err(|e| AccessError::database("count grants", project_id, e))?;
        let rows = pages
            .fetch_page(page - 1)
            .await
            .map_err(|e| AccessError::database("list grants", project_id, e))?;
        let now = Utc::now();
        let items = rows
            .into_iter()
            .map(|(grant, service)| {
                let service = service
                    .ok_or_else(|| not_found("service", grant.service_id.clone(), project_id))?;
                grant_response(grant, &service, sandbox_id, now)
            })
            .collect::<Result<_, _>>()?;
        Ok(ServiceGrantList {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn request_grant(
        &self,
        project_id: i32,
        sandbox_id: &str,
        user_id: i32,
        admin: bool,
        request: RequestServiceGrant,
    ) -> Result<ServiceGrantResponse, AccessError> {
        if !(60..=86400).contains(&request.ttl_seconds) {
            return Err(invalid("ttl_seconds", "must be between 60 and 86400"));
        }
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin grant request", project_id, e))?;
        let service = services::Entity::find_by_id(&request.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock requested service", project_id, e))?
            .ok_or_else(|| not_found("service", request.service_id.clone(), project_id))?;
        if service.version != request.service_version {
            return Err(AccessError::Conflict {
                id: service.id,
                version: request.service_version,
            });
        }
        if !service.enabled {
            return Err(invalid("service_id", "this service is disabled"));
        }
        if request.ttl_seconds > service.max_ttl_seconds {
            return Err(invalid(
                "ttl_seconds",
                "exceeds the service maximum lifetime",
            ));
        }
        let sandbox = self
            .sandbox(&tx, project_id, sandbox_id, user_id, admin)
            .await?;
        if sandbox.status == "destroyed" {
            return Err(invalid(
                "sandbox_id",
                "a destroyed sandbox cannot request access",
            ));
        }
        let now = Utc::now();
        let grant = grants::ActiveModel {
            id: Set(format!("grant_{}", uuid::Uuid::new_v4().simple())),
            project_id: Set(project_id),
            service_id: Set(service.id.clone()),
            sandbox_id: Set(sandbox.id),
            service_version: Set(service.version),
            policy_snapshot: Set(service.policy.clone()),
            created_by: Set(Some(user_id)),
            version: Set(1),
            created_at: Set(now),
            expires_at: Set(now + Duration::seconds(i64::from(request.ttl_seconds))),
            revoked_at: Set(None),
        }
        .insert(&tx)
        .await
        .map_err(|e| AccessError::database("save grant request", project_id, e))?;
        let result = grant_response(grant, &service, sandbox_id, now)?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit grant request", project_id, e))?;
        Ok(result)
    }

    pub async fn revoke_grant(
        &self,
        project_id: i32,
        sandbox_id: &str,
        grant_id: &str,
        user_id: i32,
        admin: bool,
        version: i32,
    ) -> Result<ServiceGrantResponse, AccessError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin request revocation", project_id, e))?;
        let sandbox = self
            .sandbox(&tx, project_id, sandbox_id, user_id, admin)
            .await?;
        let grant = grants::Entity::find_by_id(grant_id)
            .filter(grants::Column::ProjectId.eq(project_id))
            .filter(grants::Column::SandboxId.eq(sandbox.id))
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock grant request", project_id, e))?
            .ok_or_else(|| not_found("grant", grant_id.into(), project_id))?;
        if grant.version != version || version == i32::MAX {
            return Err(AccessError::Conflict {
                id: grant_id.into(),
                version,
            });
        }
        let service = services::Entity::find_by_id(&grant.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("find grant service", project_id, e))?
            .ok_or_else(|| not_found("service", grant.service_id.clone(), project_id))?;
        // These are never-provisioned intents. Future enforcement must use a
        // separate revoke/ack state machine, not this database-only operation.
        let now = Utc::now();
        let grant = if grant.revoked_at.is_none() {
            let mut model = grant.into_active_model();
            model.revoked_at = Set(Some(now));
            model.version = Set(version + 1);
            model
                .update(&tx)
                .await
                .map_err(|e| AccessError::database("revoke grant request", project_id, e))?
        } else {
            grant
        };
        let result = grant_response(grant, &service, sandbox_id, now)?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit request revocation", project_id, e))?;
        Ok(result)
    }
}

fn invalid(field: &'static str, reason: &'static str) -> AccessError {
    AccessError::Validation { field, reason }
}
fn not_found(kind: &'static str, id: String, project_id: i32) -> AccessError {
    AccessError::NotFound {
        kind,
        id,
        project_id,
    }
}

pub fn validate_service(request: &mut SaveAccessService) -> Result<(), AccessError> {
    request.name = request.name.trim().into();
    if request.name.is_empty()
        || request.name.chars().count() > 80
        || request.name.chars().any(char::is_control)
    {
        return Err(invalid("name", "must contain 1–80 printable characters"));
    }
    if !(60..=86400).contains(&request.max_ttl_seconds) {
        return Err(invalid("max_ttl_seconds", "must be between 60 and 86400"));
    }
    if request.policy.github_repository.is_some()
        && (request.credential.is_some() || request.clear_credential)
    {
        return Err(invalid("credential", "GitHub services use the project connection; do not supply or clear a bearer credential"));
    }
    if request.clear_credential && request.credential.is_some() {
        return Err(invalid(
            "credential",
            "cannot set and clear a credential together",
        ));
    }
    if let Some(secret) = &request.credential {
        let secret = secret.expose();
        if secret.is_empty()
            || secret.len() > 16384
            || !secret.bytes().all(|c| (33..=126).contains(&c))
        {
            return Err(invalid("credential", "must be a nonempty bearer value of at most 16384 printable ASCII bytes, without whitespace"));
        }
    }
    let (policy, _) = crate::tokenless::compile_policy(&request.policy)?;
    request.policy = policy;
    Ok(())
}

fn service_response(model: services::Model) -> Result<AccessServiceResponse, AccessError> {
    let policy: SandboxAccessPolicy =
        serde_json::from_value(model.policy).map_err(|source| AccessError::StoredPolicy {
            id: model.id.clone(),
            project_id: model.project_id,
            source,
        })?;
    Ok(AccessServiceResponse {
        id: model.id,
        project_id: model.project_id,
        name: model.name,
        has_credential: model.credential_encrypted.is_some() || policy.github_repository.is_some(),
        policy,
        max_ttl_seconds: model.max_ttl_seconds,
        enabled: model.enabled,
        version: model.version,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn grant_response(
    grant: grants::Model,
    service: &services::Model,
    sandbox_id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<ServiceGrantResponse, AccessError> {
    let (status, reason) = if grant.revoked_at.is_some() {
        (
            ServiceGrantStatus::Revoked,
            "This saved request was revoked.",
        )
    } else if grant.expires_at <= now {
        (
            ServiceGrantStatus::Expired,
            "This saved request expired. Save a new request to choose a new lifetime.",
        )
    } else if !service.enabled {
        (
            ServiceGrantStatus::ServiceDisabled,
            "The service is disabled.",
        )
    } else if grant.service_version != service.version {
        (
            ServiceGrantStatus::PolicyChanged,
            "The service changed. Review its policy and save a new request.",
        )
    } else {
        (
            ServiceGrantStatus::IntegrationUnavailable,
            crate::broker::UNAVAILABLE_REASON,
        )
    };
    let policy = serde_json::from_value(grant.policy_snapshot).map_err(|source| {
        AccessError::StoredPolicy {
            id: grant.id.clone(),
            project_id: grant.project_id,
            source,
        }
    })?;
    Ok(ServiceGrantResponse {
        id: grant.id,
        service_id: service.id.clone(),
        service_name: service.name.clone(),
        sandbox_id: sandbox_id.into(),
        service_version: grant.service_version,
        policy,
        status,
        reason: reason.into(),
        connected: false,
        version: grant.version,
        created_at: grant.created_at,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
    })
}

#[cfg(test)]
mod tests;
