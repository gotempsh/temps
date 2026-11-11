// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use temps_entities::{
    sandbox_access_authorizations as authorizations, sandbox_access_runtimes as runtimes,
};

#[derive(FromQueryResult)]
struct Approver {
    admin: bool,
}
pub(crate) struct ApprovalBatch {
    pub assignments: Vec<assignments::Model>,
    pub processed: usize,
}
impl ServiceAccessService {
    /// Indexed, bounded claims. Claiming never authorizes traffic. A crashed
    /// reconciler retries after 30s; stale work must pass the transaction again.
    pub(crate) async fn due_assignments(&self) -> Result<ApprovalBatch, AccessError> {
        // Limit candidates before testing terminal conditions, so expired history
        // cannot consume an unbounded scan on every poll. NULL leaves the indexed
        // next_reconcile_at <= now() range; explicit reapproval schedules it again.
        let rows = assignments::Model::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
WITH due AS (
 SELECT id FROM sandbox_service_assignments
 WHERE approved_at IS NOT NULL AND revoked_at IS NULL AND next_reconcile_at<=now()
 ORDER BY next_reconcile_at,id LIMIT 8 FOR UPDATE SKIP LOCKED
)
UPDATE sandbox_service_assignments a SET next_reconcile_at=CASE
 WHEN (a.expires_at IS NOT NULL AND a.expires_at<=now()+interval '1 second')
   OR NOT EXISTS (SELECT 1 FROM users u WHERE u.id=a.approved_by AND u.deleted_at IS NULL)
 THEN NULL ELSE LEAST(now()+interval '30 seconds',a.expires_at-interval '1 second') END
FROM due WHERE a.id=due.id RETURNING a.*
"#,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|e| AccessError::database("claim due service approvals", 0, e))?;
        let processed = rows.len();
        Ok(ApprovalBatch {
            assignments: rows
                .into_iter()
                .filter(|row| row.next_reconcile_at.is_some())
                .collect(),
            processed,
        })
    }
    pub(crate) async fn assignment_sandbox(
        &self,
        row: &assignments::Model,
    ) -> Result<Option<sandboxes::Model>, AccessError> {
        sandboxes::Entity::find_by_id(row.sandbox_id)
            .filter(sandboxes::Column::ProjectId.eq(row.project_id))
            .filter(sandboxes::Column::Status.eq("running"))
            .one(self.db.as_ref())
            .await
            .map_err(|e| AccessError::database("resolve approved workload", row.project_id, e))
    }
    /// The row is locked again after host inspection. Revocation, fixed expiry,
    /// policy changes and approver deletion cannot race a successful renewal.
    pub(crate) async fn renew_assignment(
        &self,
        id: &str,
        project_id: i32,
        sandbox_id: i32,
        observed: HostObservation,
    ) -> Result<Option<String>, AccessError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(|e| AccessError::database("begin approval renewal", project_id, e))?;
        let row = self.assignment(&tx, project_id, sandbox_id, id).await?;
        let now = Utc::now();
        if row.revoked_at.is_some()
            || row.approved_at.is_none()
            || row
                .expires_at
                .is_some_and(|e| e <= now + Duration::seconds(1))
        {
            return Ok(None);
        }
        let Some(user_id) = row.approved_by else {
            return Ok(None);
        };
        let approver=Approver::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,"SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.name='admin') AS admin FROM users u WHERE u.id=$1 AND u.deleted_at IS NULL",[user_id.into()])).one(&tx).await.map_err(|e|AccessError::database("recheck approval principal",project_id,e))?;
        let Some(approver) = approver else {
            return Ok(None);
        };
        self.project(&tx, project_id).await?;
        let service = services::Entity::find_by_id(&row.service_id)
            .filter(services::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock renewal policy", project_id, e))?
            .ok_or_else(|| not_found("service", row.service_id.clone(), project_id))?;
        if !service.enabled || service.version != row.service_version {
            return Ok(None);
        };
        // A durable standalone approval does not become vault consent when its
        // service is added to a vault. Stop both extension and grant rotation.
        match self.require_standalone_service(&tx, &service.id).await {
            Ok(()) => {}
            Err(AccessError::VaultSessionRequired { .. }) => {
                let next_version = row.version + 1;
                let mut retired = row.into_active_model();
                retired.version = Set(next_version);
                retired.revoked_at = Set(Some(now));
                retired.next_reconcile_at = Set(None);
                retired.updated_at = Set(now);
                retired.update(&tx).await.map_err(|e| {
                    AccessError::database(
                        "retire standalone approval after vault membership",
                        project_id,
                        e,
                    )
                })?;
                tx.commit().await.map_err(|e| {
                    AccessError::database("commit retired standalone approval", project_id, e)
                })?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        let sandbox = self
            .sandbox(
                &tx,
                project_id,
                &self.sandbox_public_id(&tx, sandbox_id, project_id).await?,
                user_id,
                approver.admin,
            )
            .await?;
        if sandbox.status != "running" || sandbox.node_id != observed.node_id {
            return Ok(None);
        };
        // Match consent's sandbox -> runtime -> authority ordering before a
        // grant trigger can update the shared authority.
        let locked_sandbox = sandboxes::Entity::find_by_id(sandbox_id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("lock renewal sandbox", project_id, e))?;
        if locked_sandbox
            .is_none_or(|row| row.status != "running" || row.node_id != observed.node_id)
        {
            return Ok(None);
        }
        let runtime = runtimes::Entity::find()
            .filter(runtimes::Column::SandboxId.eq(sandbox_id))
            .filter(runtimes::Column::ClosedAt.is_null())
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(|e| AccessError::database("recheck renewal incarnation", project_id, e))?;
        let ttl = service.max_ttl_seconds;
        let expires = (now + Duration::seconds(i64::from(ttl))).min(
            row.expires_at
                .unwrap_or(now + Duration::seconds(i64::from(ttl))),
        );
        let existing = if let Some(grant_id) = &row.grant_id {
            grants::Entity::find_by_id(grant_id)
                .filter(grants::Column::AssignmentId.eq(id))
                .filter(grants::Column::RevokedAt.is_null())
                .one(&tx)
                .await
                .map_err(|e| AccessError::database("read maintained grant", project_id, e))?
        } else {
            None
        };
        if let (Some(runtime), Some(grant)) = (runtime, existing) {
            let auth = authorizations::Entity::find_by_id(&grant.id)
                .one(&tx)
                .await
                .map_err(|e| AccessError::database("read maintained binding", project_id, e))?;
            if auth.is_some_and(|a| a.runtime_id == runtime.id)
                && runtime.id == observed.incarnation
                && runtime.authority_id == Some(observed.worker.boot_id)
                && runtime.provider_identity == observed.container_id
                && runtime.node_id == observed.node_id
                && grant.expires_at > now
                // A grant's creation time is immutable. Rotate before extending
                // past its database lifetime cap, while the old grant is live.
                && (grant.expires_at >= now + Duration::seconds(45)
                    || expires <= grant.created_at + Duration::hours(24))
            {
                // Extending internal authorization is permitted only by the
                // durable approval locked above. The worker still gets 30s leases.
                if grant.expires_at < now + Duration::seconds(45) {
                    let mut model = grant.into_active_model();
                    model.expires_at = Set(expires);
                    model.update(&tx).await.map_err(|e| {
                        AccessError::database("renew internal grant deadline", project_id, e)
                    })?;
                }
                tx.commit()
                    .await
                    .map_err(|e| AccessError::database("commit lease renewal", project_id, e))?;
                return Ok(None);
            }
        }
        cut_assignment_grants(&tx, project_id, id).await?;
        // The short underlying grant is created through the same host-observation
        // checks as manual consent. Its origin stays linked to this approval.
        let consent = FreshConsent {
            request_id: uuid::Uuid::new_v4(),
            project_id,
            sandbox_id: sandbox.public_id,
            user_id,
            admin: approver.admin,
            request: RequestServiceGrant {
                service_id: service.id,
                service_version: row.service_version,
                ttl_seconds: ttl,
            },
        };
        let grant = self
            .authorize_grant_in_transaction(&tx, consent, observed, now)
            .await?;
        grants::Entity::update_many()
            .col_expr(grants::Column::AssignmentId, Expr::value(id))
            .col_expr(grants::Column::ExpiresAt, Expr::value(expires))
            .filter(grants::Column::Id.eq(&grant.grant_id))
            .exec(&tx)
            .await
            .map_err(|e| AccessError::database("bind grant to durable approval", project_id, e))?;
        let mut model = row.into_active_model();
        model.grant_id = Set(Some(grant.grant_id.clone()));
        model.updated_at = Set(now);
        model
            .update(&tx)
            .await
            .map_err(|e| AccessError::database("save active approval binding", project_id, e))?;
        tx.commit()
            .await
            .map_err(|e| AccessError::database("commit approval binding", project_id, e))?;
        Ok(Some(grant.grant_id))
    }
    async fn sandbox_public_id<C: ConnectionTrait>(
        &self,
        db: &C,
        id: i32,
        project_id: i32,
    ) -> Result<String, AccessError> {
        sandboxes::Entity::find_by_id(id)
            .filter(sandboxes::Column::ProjectId.eq(project_id))
            .select_only()
            .column(sandboxes::Column::PublicId)
            .into_tuple::<String>()
            .one(db)
            .await
            .map_err(|e| AccessError::database("resolve approval sandbox", project_id, e))?
            .ok_or_else(|| not_found("sandbox", id.to_string(), project_id))
    }
}
