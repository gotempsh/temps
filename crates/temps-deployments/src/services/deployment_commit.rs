// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ActiveModelTrait, EntityTrait, QuerySelect, Set, TransactionTrait};
use temps_database::DbConnection;
use temps_entities::{audit_logs, deployments};
use temps_git::services::git_ops::HeadCommit;

/// Whether a deployment's stored commit still needs the checked-out SHA.
///
/// Deployments queued without a known commit (a manual deploy, or the first
/// deployment of a public repository with no provider API to ask) are stored
/// with no commit; older ones recorded the symbolic ref `HEAD`. Anything else
/// came from a webhook or a user and is left exactly as it is.
pub(crate) fn is_unresolved_commit(stored: Option<&str>) -> bool {
    match stored.map(str::trim) {
        None => true,
        Some(commit) => commit.is_empty() || commit.eq_ignore_ascii_case("HEAD"),
    }
}

/// Record the commit a deployment actually checked out when it was queued
/// without one. Returns whether the deployment was updated.
///
/// The commit message and author are filled only when they are missing, so
/// details fetched from the provider API are never replaced.
pub(crate) async fn record_checked_out_commit(
    db: &DbConnection,
    deployment_id: i32,
    commit: &HeadCommit,
) -> Result<bool, sea_orm::DbErr> {
    let transaction = db.begin().await?;
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .one(&transaction)
        .await?
    else {
        return Ok(false);
    };
    if !is_unresolved_commit(deployment.commit_sha.as_deref()) {
        return Ok(false);
    }

    let project_id = deployment.project_id;
    let environment_id = deployment.environment_id;
    let previous_sha = deployment.commit_sha.clone();
    let fill_message = deployment.commit_message.is_none();
    let fill_author = deployment.commit_author.is_none();
    let mut active: deployments::ActiveModel = deployment.into();
    active.commit_sha = Set(Some(commit.sha.clone()));
    if fill_message {
        if let Some(message) = commit.message.clone() {
            active.commit_message = Set(Some(message));
        }
    }
    if fill_author {
        if let Some(author) = commit.author.clone() {
            active.commit_author = Set(Some(author));
        }
    }
    active.updated_at = Set(chrono::Utc::now());
    active.update(&transaction).await?;
    let now = chrono::Utc::now();
    let audit = audit_logs::ActiveModel {
        user_id: Set(None),
        user_agent: Set("temps-deployment-workflow".to_string()),
        operation_type: Set("DEPLOYMENT_COMMIT_RESOLVED".to_string()),
        audit_date: Set(now),
        // Entity::insert bypasses ActiveModelBehavior::before_save.
        created_at: Set(now),
        data: Set(serde_json::json!({
            "deployment_id": deployment_id,
            "project_id": project_id,
            "environment_id": environment_id,
            "previous_commit_sha": previous_sha,
            "commit_sha": commit.sha,
            "actor": "system",
        })
        .to_string()),
        ..Default::default()
    };
    audit_logs::Entity::insert(audit)
        .exec_without_returning(&transaction)
        .await?;
    transaction.commit().await?;
    Ok(true)
}
