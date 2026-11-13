// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{WorkflowService, MAX_WORKFLOW_NAME};
use crate::{
    error::{WorkflowError, WorkflowResult},
    store::{RunFilter, RunRow},
    types::validate_name,
};
use chrono::{DateTime, Utc};
use temps_authz::{AuthContext, Permission};
use temps_core::ProjectAccessChecker;
use uuid::Uuid;

pub struct GlobalRuns {
    pub runs: Vec<RunRow>,
    pub next: Option<String>,
}

impl WorkflowService {
    /// Scan at most 1,000 candidates per request, authorizing in bounded batches.
    /// The cursor can advance through an empty page of inaccessible projects;
    /// neither counts nor details of those projects are returned.
    pub async fn global_runs(
        &self,
        auth: &AuthContext,
        checker: Option<&dyn ProjectAccessChecker>,
        filter: &RunFilter,
        cursor: Option<&str>,
        limit: u64,
    ) -> WorkflowResult<GlobalRuns> {
        if let Some(name) = &filter.workflow_name {
            validate_name("workflow", name, MAX_WORKFLOW_NAME)?;
        }
        let scope = crate::payload::PayloadScope {
            project_id: 0,
            environment_id: 0,
            run_id: Uuid::nil(),
        };
        let settings = crate::types::WorkflowSettings::default();
        let mut before: Option<(DateTime<Utc>, Uuid)> = if let Some(cursor) = cursor {
            if cursor.len() > 1024 {
                return Err(invalid_cursor());
            }
            let value = self
                .codec
                .open(
                    &scope,
                    &crate::payload::PayloadField::RunInput,
                    &crate::payload::StoredPayload {
                        encrypted: Some(cursor.into()),
                        ..Default::default()
                    },
                    &settings,
                )
                .await
                .map_err(|_| invalid_cursor())?
                .ok_or_else(invalid_cursor)?;
            Some(serde_json::from_value(value).map_err(|_| invalid_cursor())?)
        } else {
            None
        };
        let limit = limit.clamp(1, 100) as usize;
        let mut runs = Vec::with_capacity(limit);
        for _ in 0..10 {
            let rows = self.store.scan_runs(filter, before, 100).await?;
            let ids: Vec<i32> = rows.iter().map(|r| r.project_id).collect();
            let visible = visible_projects(auth, checker, &ids).await?;
            let exhausted = rows.len() < 100;
            for row in rows {
                before = Some((row.created_at, row.id));
                if visible.contains(&row.project_id) {
                    runs.push(row);
                }
                if runs.len() == limit {
                    return Ok(GlobalRuns {
                        runs,
                        next: self.global_cursor(before).await?,
                    });
                }
            }
            if exhausted {
                return Ok(GlobalRuns { runs, next: None });
            }
        }
        Ok(GlobalRuns {
            runs,
            next: self.global_cursor(before).await?,
        })
    }
    async fn global_cursor(
        &self,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> WorkflowResult<Option<String>> {
        let Some((time, id)) = before else {
            return Ok(None);
        };
        let sealed = self
            .codec
            .seal(
                &crate::payload::PayloadScope {
                    project_id: 0,
                    environment_id: 0,
                    run_id: Uuid::nil(),
                },
                &crate::payload::PayloadField::RunInput,
                &serde_json::json!([time, id]),
                0,
                &crate::types::WorkflowSettings::default(),
            )
            .await?;
        Ok(sealed.encrypted)
    }
}

async fn visible_projects(
    auth: &AuthContext,
    checker: Option<&dyn ProjectAccessChecker>,
    ids: &[i32],
) -> WorkflowResult<std::collections::BTreeSet<i32>> {
    let unique: std::collections::BTreeSet<i32> = ids.iter().copied().collect();
    let Some(checker) = checker.filter(|_| !auth.is_instance_admin()) else {
        return Ok(unique);
    };
    let user = auth
        .user_id_opt()
        .ok_or(WorkflowError::ListingAccess { user_id: None })?;
    let ids: Vec<i32> = unique.iter().copied().collect();
    let permissions = checker
        .effective_project_permissions_batch(user, &ids)
        .await
        .map_err(|_| WorkflowError::ListingAccess {
            user_id: Some(user),
        })?;
    let fallback: Vec<i32> = ids
        .iter()
        .copied()
        .filter(|id| matches!(permissions.get(id), Some(None)))
        .collect();
    let coarse = checker
        .user_can_access_projects(user, &fallback)
        .await
        .map_err(|_| WorkflowError::ListingAccess {
            user_id: Some(user),
        })?;
    let permission = Permission::WorkflowsRead.to_string();
    Ok(unique
        .into_iter()
        .filter(|id| match permissions.get(id) {
            Some(Some(perms)) => perms.contains(&permission),
            Some(None) => coarse.get(id) == Some(&true),
            None => false,
        })
        .collect())
}

fn invalid_cursor() -> WorkflowError {
    WorkflowError::Validation {
        field: "cursor",
        reason: "invalid workflow listing cursor".into(),
    }
}
