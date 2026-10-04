// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use temps_database::DbConnection;
use temps_entities::deployments;
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
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if !is_unresolved_commit(deployment.commit_sha.as_deref()) {
        return Ok(false);
    }

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
    active.update(db).await?;
    Ok(true)
}
