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
            .order_by_desc(sea_orm::sea_query::Expr::col(runtimes::Column::AppliedVersion).gt(0))
            .order_by_asc(runtimes::Column::CreatedAt)
            .order_by_asc(runtimes::Column::Id)
            .limit((MAX_WORKER_BINDINGS + 1) as u64)
            .all(&tx)
            .await
            .map_err(|e| database(boot_id, "read current incarnations", e))?;
        let runtime_ids: Vec<Uuid> = runtimes.iter().map(|row| row.id).collect();
        // The joined read excludes legacy grants, deleted users/projects, moved
        // sandboxes and changed service policies. EXISTS mirrors current session
        // role resolution (admin role, otherwise User), without a query per row.
        let grants = Candidate::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            r#"SELECT r.id AS runtime_id,
               CASE WHEN s.policy->'github_repository' IS NULL AND EXISTS (
                   SELECT 1 FROM access_sessions session JOIN access_vault_links link ON link.id=COALESCE(session.link_id,CASE WHEN session.legacy_project_link THEN 'link_legacy_' || session.vault_id END)
                   WHERE session.grant_ids @> ARRAY[g.id] AND link.sandbox_id=b.id
                     AND link.revoked_at IS NULL AND session.revoked_at IS NULL
               ) THEN NULL ELSE a.project_id END AS project_id, a.approved_by,
               EXISTS(SELECT 1 FROM user_roles ur JOIN roles role ON role.id=ur.role_id
                      WHERE ur.user_id=a.approved_by AND role.name='admin') AS admin,
               g.id AS grant_id, g.service_id, g.service_version, g.policy_snapshot AS policy,
               s.max_ttl_seconds, g.expires_at, s.credential_encrypted
            FROM sandbox_access_authorizations a
            JOIN sandbox_service_grants g ON g.id=a.grant_id AND g.project_id IS NOT DISTINCT FROM a.project_id AND g.sandbox_id=a.sandbox_id
            JOIN sandbox_access_services s ON s.id=g.service_id
            JOIN sandbox_access_runtime_records r ON r.id=a.runtime_id AND r.project_id IS NOT DISTINCT FROM a.project_id AND r.sandbox_id=a.sandbox_id
            JOIN sandboxes b ON b.id=r.sandbox_id AND b.project_id IS NOT DISTINCT FROM r.project_id
            LEFT JOIN projects p ON p.id=a.project_id
            JOIN users u ON u.id=a.approved_by
            LEFT JOIN git_provider_connections c ON to_jsonb(c.id)=s.policy->'github_repository'->'connection_id'
            LEFT JOIN git_providers v ON v.id=c.provider_id
            WHERE r.authority_id=$1 AND r.closed_at IS NULL AND r.id=ANY($4::uuid[])
              AND r.node_id IS NOT DISTINCT FROM $2
              AND b.node_id IS NOT DISTINCT FROM r.node_id AND b.status='running'
              AND b.backend=r.provider_backend AND r.provider_name='temps-sandbox-' ||
                  CASE WHEN b.agent_run_id IS NOT NULL THEN b.agent_run_id::text
                       WHEN left(b.public_id,4)='sbx_' THEN substring(b.public_id FROM 5)
                       ELSE b.public_id END
              AND (a.project_id IS NULL OR (NOT p.is_deleted AND p.deleted_at IS NULL)) AND u.deleted_at IS NULL
              AND (b.user_id=a.approved_by OR EXISTS(
                  SELECT 1 FROM user_roles ur JOIN roles role ON role.id=ur.role_id
                  WHERE ur.user_id=a.approved_by AND role.name='admin'))
              AND g.revoked_at IS NULL AND g.expires_at > now() + interval '1 second'
              AND s.enabled AND (
                (s.policy->'github_repository' IS NULL AND s.credential_encrypted IS NOT NULL) OR
                (s.policy->'github_repository'->'connection_id'=to_jsonb(c.id)
                 AND EXISTS (SELECT 1 FROM repositories repo WHERE repo.git_provider_connection_id=c.id
                     AND lower(repo.owner)=lower(s.policy->'github_repository'->>'owner')
                     AND lower(repo.name)=lower(s.policy->'github_repository'->>'repository'))
                 AND s.project_id=a.project_id AND c.is_active AND NOT c.is_expired AND v.is_active AND v.provider_type='github'
                 AND (v.base_url IS NULL OR v.base_url IN ('https://github.com','https://github.com/'))
                 AND (v.api_url IS NULL OR v.api_url IN ('https://api.github.com','https://api.github.com/'))))
              AND (g.assignment_id IS NULL OR EXISTS (
                SELECT 1 FROM sandbox_service_assignments d WHERE d.id=g.assignment_id
                  AND d.project_id=g.project_id AND d.sandbox_id=g.sandbox_id
                  AND d.service_id=g.service_id AND d.service_version=g.service_version
                  AND d.approved_by=a.approved_by AND d.approved_at IS NOT NULL
                  AND d.revoked_at IS NULL AND (d.expires_at IS NULL OR d.expires_at>now()+interval '1 second')
                  AND d.grant_id=g.id))
              AND (NOT EXISTS (SELECT 1 FROM access_sessions session WHERE session.grant_ids @> ARRAY[g.id]) OR EXISTS (
                SELECT 1 FROM access_sessions session JOIN access_vault_links link ON link.id=COALESCE(session.link_id,CASE WHEN session.legacy_project_link THEN 'link_legacy_' || session.vault_id END)
                WHERE session.grant_ids @> ARRAY[g.id] AND session.revoked_at IS NULL AND session.expires_at>now()
                  AND link.revoked_at IS NULL AND link.vault_id=session.vault_id
                  AND (link.sandbox_id=b.id OR link.project_id=b.project_id)))
              AND s.version=g.service_version AND s.policy=g.policy_snapshot
            ORDER BY r.id,g.id LIMIT $3"#,
            [boot_id.into(), authority.node_id.into(), ((MAX_GRANTS + 1) as i64).into(), runtime_ids.into()]))
            .all(&tx).await.map_err(|e| database(boot_id, "read eligible approved grants", e))?;
        if grants.len() > MAX_GRANTS {
            return Err(invalid(
                boot_id,
                "read eligible approved grants",
                "worker scan exceeds the bounded grant limit",
            ));
        }
        tx.commit()
            .await
            .map_err(|e| database(boot_id, "finish authorization snapshot", e))?;
        Ok((authority, runtimes, grants))
    }
}
