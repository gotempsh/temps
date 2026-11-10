// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fresh operator consent. The caller obtains the observation through the
//! authenticated provider before this transaction; it is never an HTTP DTO.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::SubsecRound;
use temps_entities::{
    sandbox_access_authorities as authorities, sandbox_access_authorizations as authorizations,
    sandbox_access_runtimes as runtimes,
};
use temps_service_access_runtime::WorkerIdentity;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HostObservation {
    pub worker: WorkerIdentity,
    pub incarnation: Uuid,
    pub node_id: Option<i32>,
    pub container_id: String,
    pub container_name: String,
    pub backend: String,
}

#[derive(Debug)]
pub struct FreshConsent {
    /// Stable across retries of one explicit approval; never reused for renewal.
    pub request_id: Uuid,
    pub project_id: i32,
    pub sandbox_id: String,
    pub user_id: i32,
    pub admin: bool,
    pub request: RequestServiceGrant,
}

impl FreshConsent {
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.request_id.is_nil()
            || self.user_id <= 0
            || !(60..=86400).contains(&self.request.ttl_seconds)
        {
            return Err(AccessError::Validation { field: "consent", reason: "a non-nil request ID, user and lifetime between 60 and 86400 seconds are required" });
        }
        Ok(())
    }
}

/// Internal receipt of persisted intent, not evidence that traffic is enabled.
#[derive(Debug)]
pub struct AuthorizedGrant {
    pub grant_id: String,
    pub runtime_id: Uuid,
    pub desired_version: i64,
    pub expires_at: chrono::DateTime<Utc>,
}

/// Service rows must already be locked in ID order in this transaction.
pub(super) struct LockedConsentBatch {
    pub project_id: i32,
    pub sandbox_id: String,
    pub user_id: i32,
    pub admin: bool,
    pub selections: Vec<(Uuid, RequestServiceGrant, services::Model)>,
}

impl ServiceAccessService {
    /// Called only after the handler checks effective project AND sandbox write
    /// permission and records the resulting grant in its audit log. Existing
    /// saved intentions are never passed in or converted into authorizations.
    pub async fn authorize_grant(
        &self,
        consent: FreshConsent,
        observed: HostObservation,
    ) -> Result<AuthorizedGrant, AccessError> {
        consent.validate()?;
        validate_observation(&observed)?;
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin fresh grant", consent.project_id, e))?;
        let project_id = consent.project_id;
        let result = self
            .authorize_grant_in_transaction(&tx, consent, observed, Utc::now())
            .await?;
        tx.commit().await.map_err(|e| {
            AccessError::database("commit fresh sandbox authorization", project_id, e)
        })?;
        Ok(result)
    }

    pub(super) async fn authorize_grant_in_transaction(
        &self,
        tx: &sea_orm::DatabaseTransaction,
        consent: FreshConsent,
        observed: HostObservation,
        issued_at: chrono::DateTime<Utc>,
    ) -> Result<AuthorizedGrant, AccessError> {
        let FreshConsent {
            request_id,
            project_id,
            sandbox_id,
            user_id,
            admin,
            request,
        } = consent;
        let sandbox_id = sandbox_id.as_str();
        if request_id.is_nil() || user_id <= 0 || !(60..=86400).contains(&request.ttl_seconds) {
            return Err(invalid(
                "consent",
                "a user and a lifetime between 60 and 86400 seconds are required",
            ));
        }
        validate_observation(&observed)?;
        self.project(tx, project_id).await?;
        let service = services::Entity::find_by_id(&request.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(tx)
            .await
            .map_err(|e| AccessError::database("lock approved service", project_id, e))?
            .ok_or_else(|| not_found("service", request.service_id.clone(), project_id))?;
        let mut grants = self
            .authorize_locked_grants(
                tx,
                LockedConsentBatch {
                    project_id,
                    sandbox_id: sandbox_id.into(),
                    user_id,
                    admin,
                    selections: vec![(request_id, request, service)],
                },
                observed,
                issued_at,
            )
            .await?;
        grants
            .pop()
            .ok_or_else(|| invalid("grants", "single approval produced no grant"))
    }

    pub(super) async fn authorize_locked_grants(
        &self,
        tx: &sea_orm::DatabaseTransaction,
        batch: LockedConsentBatch,
        observed: HostObservation,
        issued_at: chrono::DateTime<Utc>,
    ) -> Result<Vec<AuthorizedGrant>, AccessError> {
        let LockedConsentBatch {
            project_id,
            sandbox_id,
            user_id,
            admin,
            selections,
        } = batch;
        if selections.is_empty()
            || selections.len() > crate::tokenless::MAX_BINDING_GRANTS
            || user_id <= 0
        {
            return Err(invalid(
                "consent",
                "a user and between 1 and 16 selected grants are required",
            ));
        }
        validate_observation(&observed)?;
        let sandbox_id = sandbox_id.as_str();
        let mut github_sources = Vec::new();
        for (request_id, request, service) in &selections {
            if request_id.is_nil()
                || !(60..=86400).contains(&request.ttl_seconds)
                || service.project_id != project_id
                || service.id != request.service_id
            {
                return Err(invalid(
                    "consent",
                    "selected service must match the project and approved request",
                ));
            }
            if service.version != request.service_version {
                return Err(AccessError::Conflict {
                    id: service.id.clone(),
                    version: request.service_version,
                });
            }
            if !service.enabled {
                return Err(invalid(
                    "service_id",
                    "the service must be enabled and have a credential before connecting",
                ));
            }
            if request.ttl_seconds > service.max_ttl_seconds {
                return Err(invalid(
                    "ttl_seconds",
                    "exceeds the service maximum lifetime",
                ));
            }
            // Revalidate stored policy before creating live authorization.
            let policy: SandboxAccessPolicy = serde_json::from_value(service.policy.clone())
                .map_err(|source| AccessError::StoredPolicy {
                    id: service.id.clone(),
                    project_id,
                    source,
                })?;
            crate::tokenless::compile_policy(&policy)?;
            if let Some(source) = &policy.github_repository {
                github_sources.push(source.clone());
            } else if service.credential_encrypted.is_none() {
                return Err(invalid(
                    "service_id",
                    "the service needs a credential before connecting",
                ));
            }
        }
        crate::github::validate_bindings(tx, project_id, &github_sources).await?;
        let mut sandbox_query = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(sandbox_id))
            .filter(sandboxes::Column::ProjectId.eq(project_id));
        if !admin {
            sandbox_query = sandbox_query.filter(sandboxes::Column::UserId.eq(user_id));
        }
        let sandbox = sandbox_query
            .lock_exclusive()
            .one(tx)
            .await
            .map_err(|e| AccessError::database("lock approved sandbox", project_id, e))?
            .ok_or_else(|| not_found("sandbox", sandbox_id.into(), project_id))?;
        if sandbox.status != "running"
            || sandbox.node_id != observed.node_id
            || sandbox.backend.as_deref() != Some(observed.backend.as_str())
            || observed.container_name
                != format!(
                    "temps-sandbox-{}",
                    crate::transport::sandbox_label(&sandbox)
                )
        {
            return Err(invalid(
                "sandbox_id",
                "the running sandbox no longer matches the authenticated worker observation",
            ));
        }
        if let [(request_id, request, _)] = selections.as_slice() {
            if let Some(receipt) = replay_consent(
                tx,
                *request_id,
                project_id,
                &sandbox,
                request,
                user_id,
                observed.incarnation,
            )
            .await?
            {
                return Ok(vec![receipt]);
            }
        }
        let authority = authorities::Entity::find_by_id(observed.worker.boot_id)
            .one(tx)
            .await
            .map_err(|e| AccessError::database("find approved worker authority", project_id, e))?
            .ok_or_else(|| {
                not_found(
                    "worker authority",
                    observed.worker.boot_id.to_string(),
                    project_id,
                )
            })?;
        if authority.node_id != observed.node_id
            || authority.closed_at.is_some()
            || authority.seal_public_key != STANDARD.encode(observed.worker.seal_public_key)
        {
            return Err(invalid(
                "worker",
                "worker authority changed or was retired; inspect the sandbox again",
            ));
        }
        let existing = runtimes::Entity::find()
            .filter(runtimes::Column::SandboxId.eq(sandbox.id))
            .filter(runtimes::Column::ClosedAt.is_null())
            .lock_exclusive()
            .one(tx)
            .await
            .map_err(|e| AccessError::database("lock sandbox access incarnation", project_id, e))?;
        let runtime = match existing {
            Some(row) if row.id == observed.incarnation => {
                if row.closed_at.is_some()
                    || row.authority_id != Some(authority.id)
                    || row.node_id != observed.node_id
                    || row.provider_identity != observed.container_id
                    || row.provider_backend.as_deref() != Some(observed.backend.as_str())
                    || row.provider_name.as_deref() != Some(observed.container_name.as_str())
                {
                    return Err(invalid(
                        "incarnation",
                        "sandbox binding changed or was retired; inspect again",
                    ));
                }
                row
            }
            previous => {
                if let Some(previous) = previous {
                    // Preserve historical consent and queue an exact retirement
                    // before making a new incarnation current.
                    crate::delivery::DeliveryQueue::advance(
                        tx,
                        previous.id,
                        previous.desired_version,
                        temps_entities::sandbox_access_delivery::DeliveryOperation::Retire,
                    )
                    .await
                    .map_err(|source| AccessError::Delivery {
                        project_id,
                        sandbox_id: sandbox.public_id.clone(),
                        source: Box::new(source),
                    })?;
                }
                reserve_worker_capacity(tx, authority.id, project_id).await?;
                let now = Utc::now();
                runtimes::ActiveModel {
                    id: Set(observed.incarnation),
                    project_id: Set(project_id),
                    sandbox_id: Set(sandbox.id),
                    node_id: Set(observed.node_id),
                    provider_identity: Set(observed.container_id),
                    authority_id: Set(Some(authority.id)),
                    provider_backend: Set(Some(observed.backend)),
                    provider_name: Set(Some(observed.container_name)),
                    lease_expires_at: Set(None),
                    desired_version: Set(0),
                    applied_version: Set(0),
                    applied_revision: Set(None),
                    applied_grant_ids: Set(Vec::new()),
                    closed_at: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(tx)
                .await
                .map_err(|e| {
                    AccessError::database("register approved sandbox incarnation", project_id, e)
                })?
            }
        };
        let count = tx.query_one(sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) AS count FROM sandbox_access_authorizations a JOIN sandbox_service_grants g ON g.id=a.grant_id WHERE a.runtime_id=$1 AND g.revoked_at IS NULL AND g.expires_at>now()",
            [runtime.id.into()])).await.map_err(|e| AccessError::database("count active sandbox grants", project_id, e))?
            .ok_or_else(|| invalid("grants", "cannot determine the sandbox grant count"))?
            .try_get::<i64>("", "count").map_err(|e| AccessError::database("read active sandbox grant count", project_id, e))?;
        if count + selections.len() as i64 > crate::tokenless::MAX_BINDING_GRANTS as i64 {
            return Err(invalid(
                "grants",
                "the sandbox would exceed 16 active grants; revoke grants before adding more",
            ));
        }
        // PostgreSQL retains microseconds. Use the same precision for the
        // initial receipt and persisted grants so consent replays are exact.
        let now = issued_at.trunc_subsecs(6);
        let mut grant_models = Vec::with_capacity(selections.len());
        let mut authorization_models = Vec::with_capacity(selections.len());
        let mut receipts = Vec::with_capacity(selections.len());
        for (request_id, request, service) in selections {
            let grant_id = format!("grant_{}", Uuid::new_v4().simple());
            let expires_at = now + Duration::seconds(i64::from(request.ttl_seconds));
            grant_models.push(grants::ActiveModel {
                assignment_id: Set(None),
                id: Set(grant_id.clone()),
                project_id: Set(project_id),
                service_id: Set(service.id),
                sandbox_id: Set(sandbox.id),
                service_version: Set(service.version),
                policy_snapshot: Set(service.policy),
                created_by: Set(Some(user_id)),
                version: Set(1),
                created_at: Set(now),
                expires_at: Set(expires_at),
                revoked_at: Set(None),
            });
            authorization_models.push(authorizations::ActiveModel {
                grant_id: Set(grant_id.clone()),
                request_id: Set(request_id),
                requested_ttl: Set(request.ttl_seconds),
                runtime_id: Set(runtime.id),
                project_id: Set(project_id),
                sandbox_id: Set(sandbox.id),
                approved_by: Set(Some(user_id)),
                approved_at: Set(now),
            });
            receipts.push(AuthorizedGrant {
                grant_id,
                runtime_id: runtime.id,
                desired_version: 0,
                expires_at,
            });
        }
        grants::Entity::insert_many(grant_models)
            .exec_without_returning(tx)
            .await
            .map_err(|e| AccessError::database("persist fresh sandbox grants", project_id, e))?;
        authorizations::Entity::insert_many(authorization_models)
            .exec_without_returning(tx)
            .await
            .map_err(|e| {
                AccessError::database("persist fresh sandbox authorizations", project_id, e)
            })?;
        let version = crate::delivery::DeliveryQueue::advance(
            tx,
            runtime.id,
            runtime.desired_version,
            temps_entities::sandbox_access_delivery::DeliveryOperation::Reconcile,
        )
        .await
        .map_err(|source| AccessError::Delivery {
            project_id,
            sandbox_id: sandbox.public_id.clone(),
            source: Box::new(source),
        })?;
        for receipt in &mut receipts {
            receipt.desired_version = version;
        }
        Ok(receipts)
    }
}

// Runtime locks always precede this authority lock. Keep the capacity check and
// insertion in one transaction so approvals from different sandboxes serialize.
async fn reserve_worker_capacity(
    tx: &sea_orm::DatabaseTransaction,
    boot_id: Uuid,
    project_id: i32,
) -> Result<(), AccessError> {
    let authority = authorities::Entity::find_by_id(boot_id)
        .lock_exclusive()
        .one(tx)
        .await
        .map_err(|e| AccessError::database("lock worker admission", project_id, e))?;
    if authority.is_none_or(|row| row.closed_at.is_some()) {
        return Err(invalid(
            "worker",
            "worker authority was retired; inspect the sandbox again",
        ));
    }
    let count = runtimes::Entity::find()
        .filter(runtimes::Column::AuthorityId.eq(boot_id))
        .filter(runtimes::Column::ClosedAt.is_null())
        .count(tx)
        .await
        .map_err(|e| AccessError::database("count worker bindings for admission", project_id, e))?;
    if count >= temps_service_access_runtime::MAX_WORKER_BINDINGS as u64 {
        return Err(invalid("worker", "the worker already has 128 open bindings; retire an existing binding before connecting another sandbox"));
    }
    Ok(())
}

// The sandbox lock serializes concurrent retries. A reused key never extends
// an expiry, creates a second grant, or moves consent to another incarnation.
async fn replay_consent(
    tx: &sea_orm::DatabaseTransaction,
    request_id: Uuid,
    project_id: i32,
    sandbox: &sandboxes::Model,
    request: &RequestServiceGrant,
    user_id: i32,
    incarnation: Uuid,
) -> Result<Option<AuthorizedGrant>, AccessError> {
    let authorization = authorizations::Entity::find()
        .filter(authorizations::Column::RequestId.eq(request_id))
        .one(tx)
        .await
        .map_err(|e| AccessError::database("find consent retry", project_id, e))?;
    let Some(authorization) = authorization else {
        return Ok(None);
    };
    let project_id = authorization.project_id;
    if Some(project_id) != sandbox.project_id
        || authorization.sandbox_id != sandbox.id
        || authorization.approved_by != Some(user_id)
        || authorization.runtime_id != incarnation
        || authorization.requested_ttl != request.ttl_seconds
    {
        return Err(invalid(
            "request_id",
            "approval request was already used for different consent",
        ));
    }
    let grant = grants::Entity::find_by_id(&authorization.grant_id)
        .one(tx)
        .await
        .map_err(|e| AccessError::database("load consent retry grant", project_id, e))?
        .ok_or_else(|| not_found("grant", authorization.grant_id.clone(), project_id))?;
    if grant.service_id != request.service_id || grant.service_version != request.service_version {
        return Err(invalid(
            "request_id",
            "approval request was already used for another service policy",
        ));
    }
    let runtime = runtimes::Entity::find_by_id(incarnation)
        .one(tx)
        .await
        .map_err(|e| AccessError::database("load consent retry runtime", project_id, e))?
        .ok_or_else(|| not_found("runtime", incarnation.to_string(), project_id))?;
    if runtime.closed_at.is_some() || grant.revoked_at.is_some() || grant.expires_at <= Utc::now() {
        return Err(invalid(
            "request_id",
            "the original approval ended; submit a new approval request",
        ));
    }
    Ok(Some(AuthorizedGrant {
        grant_id: grant.id,
        runtime_id: incarnation,
        desired_version: runtime.desired_version,
        expires_at: grant.expires_at,
    }))
}

fn validate_observation(observed: &HostObservation) -> Result<(), AccessError> {
    if observed.incarnation.is_nil()
        || observed.worker.boot_id.is_nil()
        || observed.node_id.is_some_and(|id| id <= 0)
        || observed.container_id.is_empty()
        || observed.container_id.len() > 512
        || observed.container_name.is_empty()
        || observed.container_name.len() > 255
        || !matches!(
            observed.backend.as_str(),
            "docker" | "firecracker" | "microsandbox"
        )
    {
        return Err(invalid(
            "worker",
            "authenticated worker observation has invalid identity or backend fields",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
