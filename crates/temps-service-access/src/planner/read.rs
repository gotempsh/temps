// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use sea_orm::{
    AccessMode, ColumnTrait, DatabaseBackend, EntityTrait, IsolationLevel, QueryFilter, QueryOrder,
    QuerySelect, Statement, TransactionTrait,
};
use temps_entities::{
    sandbox_access_authorities as authorities, sandbox_access_runtimes as runtimes,
};

impl PolicyPlanner {
    pub(super) async fn read(
        &self,
        boot_id: Uuid,
    ) -> Result<(authorities::Model, Vec<runtimes::Model>, Vec<Candidate>), PlanError> {
        let tx = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
            .map_err(|e| database(boot_id, "begin authorization snapshot", e))?;
        let authority = authorities::Entity::find_by_id(boot_id)
            .one(&tx)
            .await
            .map_err(|e| database(boot_id, "read authority", e))?
            .filter(|row| row.closed_at.is_none())
            .ok_or_else(|| {
                invalid(
                    boot_id,
                    "read authority",
                    "worker authority is missing or closed",
                )
            })?;
        let runtimes = runtimes::Entity::find()
            .filter(runtimes::Column::AuthorityId.eq(boot_id))
            .filter(runtimes::Column::ClosedAt.is_null())
            .order_by_asc(runtimes::Column::Id)
            .limit((MAX_WORKER_BINDINGS + 1) as u64)
            .all(&tx)
            .await
            .map_err(|e| database(boot_id, "read current incarnations", e))?;
        if runtimes.len() > MAX_WORKER_BINDINGS {
            return Err(invalid(
                boot_id,
                "read current incarnations",
                "worker exceeds the 128 binding limit",
            ));
        }
        // The joined read excludes legacy grants, deleted users/projects, moved
        // sandboxes and changed service policies. EXISTS mirrors current session
        // role resolution (admin role, otherwise User), without a query per row.
        let grants = Candidate::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            r#"SELECT r.id AS runtime_id, a.project_id, a.approved_by,
               EXISTS(SELECT 1 FROM user_roles ur JOIN roles role ON role.id=ur.role_id
                      WHERE ur.user_id=a.approved_by AND role.name='admin') AS admin,
               g.id AS grant_id, g.service_id, g.service_version, g.policy_snapshot AS policy,
               s.max_ttl_seconds, g.expires_at, s.credential_encrypted
            FROM sandbox_access_authorizations a
            JOIN sandbox_service_grants g ON g.id=a.grant_id AND g.project_id=a.project_id AND g.sandbox_id=a.sandbox_id
            JOIN sandbox_access_services s ON s.id=g.service_id AND s.project_id=g.project_id
            JOIN sandbox_access_runtimes r ON r.id=a.runtime_id AND r.project_id=a.project_id AND r.sandbox_id=a.sandbox_id
            JOIN sandboxes b ON b.id=r.sandbox_id AND b.project_id=r.project_id
            JOIN projects p ON p.id=a.project_id
            JOIN users u ON u.id=a.approved_by
            WHERE r.authority_id=$1 AND r.closed_at IS NULL
              AND r.node_id IS NOT DISTINCT FROM $2
              AND b.node_id IS NOT DISTINCT FROM r.node_id AND b.status='running'
              AND b.backend=r.provider_backend AND r.provider_name='temps-sandbox-' ||
                  CASE WHEN b.agent_run_id IS NOT NULL THEN b.agent_run_id::text
                       WHEN left(b.public_id,4)='sbx_' THEN substring(b.public_id FROM 5)
                       ELSE b.public_id END
              AND NOT p.is_deleted AND p.deleted_at IS NULL AND u.deleted_at IS NULL
              AND (b.user_id=a.approved_by OR EXISTS(
                  SELECT 1 FROM user_roles ur JOIN roles role ON role.id=ur.role_id
                  WHERE ur.user_id=a.approved_by AND role.name='admin'))
              AND g.revoked_at IS NULL AND g.expires_at > now() + interval '1 second'
              AND s.enabled AND s.credential_encrypted IS NOT NULL
              AND s.version=g.service_version AND s.policy=g.policy_snapshot
            ORDER BY r.id,g.id LIMIT $3"#,
            [boot_id.into(), authority.node_id.into(), ((MAX_GRANTS + 1) as i64).into()]))
            .all(&tx).await.map_err(|e| database(boot_id, "read eligible approved grants", e))?;
        if grants.len() > MAX_GRANTS {
            return Err(invalid(
                boot_id,
                "read eligible approved grants",
                "worker exceeds the 2048 grant limit",
            ));
        }
        tx.commit()
            .await
            .map_err(|e| database(boot_id, "finish authorization snapshot", e))?;
        Ok((authority, runtimes, grants))
    }
}
