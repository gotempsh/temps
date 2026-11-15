// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Display worker evidence separately from the requested authorization state.
//! A saved intent, delivery attempt or HTTP 202 is never a connectivity receipt.
use super::*;
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use std::collections::HashMap;

#[derive(Debug, FromQueryResult)]
pub(super) struct EnforcementRow {
    pub grant_id: String,
    pub lease_expires_at: Option<chrono::DateTime<Utc>>,
    pub current_receipt: bool,
    pub installed: bool,
    pub cut_confirmed: bool,
    pub closed: bool,
    pub running: bool,
}

impl ServiceAccessService {
    pub(super) async fn observe_grants(
        &self,
        project_id: impl Into<Option<i32>>,
        sandbox_id: i32,
        items: &mut [ServiceGrantResponse],
    ) -> Result<(), AccessError> {
        let project_id = project_id.into();
        if items.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = items.iter().map(|g| g.id.clone()).collect();
        let rows = EnforcementRow::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres, r#"
SELECT c.grant_id,r.lease_expires_at,
    COALESCE(r.applied_revision=a.desired_revision AND r.applied_version=r.desired_version AND a.closed_at IS NULL,false) AS current_receipt,
    c.grant_id=ANY(r.applied_grant_ids) AS installed,
    (r.closed_at IS NOT NULL AND r.applied_version=r.desired_version AND r.applied_version>0 AND r.lease_expires_at IS NULL AND cardinality(r.applied_grant_ids)=0) AS cut_confirmed,
    (r.closed_at IS NOT NULL OR a.closed_at IS NOT NULL) AS closed,
    (s.status='running' AND s.node_id IS NOT DISTINCT FROM r.node_id) AS running
FROM sandbox_access_authorizations c
JOIN sandbox_access_runtime_records r ON r.id=c.runtime_id AND r.sandbox_id=c.sandbox_id
JOIN sandbox_access_authorities a ON a.id=r.authority_id
JOIN sandboxes s ON s.id=c.sandbox_id
WHERE c.project_id IS NOT DISTINCT FROM $1 AND c.sandbox_id=$2 AND c.grant_id=ANY($3::text[])
"#, [project_id.into(),sandbox_id.into(),ids.into()]))
            .all(self.db.as_ref()).await.map_err(|e| AccessError::database("read worker enforcement receipts",project_id,e))?;
        let rows: HashMap<_, _> = rows
            .into_iter()
            .map(|row| (row.grant_id.clone(), row))
            .collect();
        let now = Utc::now();
        for grant in items {
            if let Some(row) = rows.get(&grant.id) {
                apply_receipt(grant, row, now);
            }
        }
        Ok(())
    }
}

fn apply_receipt(
    grant: &mut ServiceGrantResponse,
    row: &EnforcementRow,
    now: chrono::DateTime<Utc>,
) {
    let eligible =
        grant.status == ServiceGrantStatus::PendingApproval && !row.closed && row.running;
    if grant.status == ServiceGrantStatus::PendingApproval {
        grant.status = ServiceGrantStatus::Approved;
    }
    // Another binding can renew the worker-wide snapshot without updating this
    // runtime's receipt. Its recorded lease expiry does not prove access stopped.
    let (enforcement, reason) = if row.cut_confirmed
        || (row.current_receipt && !row.installed)
        || grant.expires_at <= now
    {
        (
            GrantEnforcement::Stopped,
            "The worker confirmed removal, or the signed access deadline has expired.",
        )
    } else if eligible
        && row.current_receipt
        && row.installed
        && row.lease_expires_at.is_some_and(|expiry| expiry > now)
    {
        (GrantEnforcement::Active, "The assigned worker acknowledged this grant. Access ends at the grant deadline or earlier on revocation.")
    } else if !eligible {
        (GrantEnforcement::Stopping, "Access is being removed. Waiting for the worker to confirm removal or for the grant deadline to expire.")
    } else {
        (GrantEnforcement::Pending, "Approval is saved. Waiting for a current acknowledgment from the assigned worker; connectivity is not yet confirmed.")
    };
    grant.connected = enforcement == GrantEnforcement::Active;
    grant.enforcement = enforcement;
    grant.reason = reason.into();
}

#[cfg(test)]
mod tests;
