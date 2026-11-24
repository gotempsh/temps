// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Comments and the activity log of an error group (ADR 111, D1.7).
//!
//! Entries are written when a group changes (a person changes its status,
//! assigns, merges or splits it, comments on it, or an event reopens it),
//! never per event. A bulk change writes its entries in one statement.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};

use super::super::types::ErrorTrackingError;
use super::assignee::Assignee;
use super::GroupWorkflowService;

/// Longest comment, in characters.
pub const MAX_COMMENT_CHARS: usize = 10_000;

/// What happened to the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// Someone wrote a comment.
    Comment,
    /// Someone changed the status.
    StatusChanged,
    /// An event reopened a resolved or ignored group.
    Regressed,
    /// Someone assigned or unassigned the group.
    Assigned,
    /// Other groups were merged into this one.
    Merged,
    /// This group was taken out of a merge.
    Unmerged,
    /// Fingerprints were split out of this group into a new one.
    Split,
    /// An issue was opened for the group or linked to it (ADR 111, D5).
    IssueLinked,
    /// A linked issue was unlinked; the issue itself is unchanged.
    IssueUnlinked,
}

impl ActivityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::StatusChanged => "status_changed",
            Self::Regressed => "regressed",
            Self::Assigned => "assigned",
            Self::Merged => "merged",
            Self::Unmerged => "unmerged",
            Self::Split => "split",
            Self::IssueLinked => "issue_linked",
            Self::IssueUnlinked => "issue_unlinked",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "comment" => Self::Comment,
            "status_changed" => Self::StatusChanged,
            "regressed" => Self::Regressed,
            "assigned" => Self::Assigned,
            "merged" => Self::Merged,
            "unmerged" => Self::Unmerged,
            "split" => Self::Split,
            "issue_linked" => Self::IssueLinked,
            "issue_unlinked" => Self::IssueUnlinked,
            _ => return None,
        })
    }
}

/// The specifics of an entry; which fields are set depends on the kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActivityDetails {
    /// `status_changed`: the new status. `regressed`: the status the group
    /// had before the event reopened it (`resolved` or `ignored`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `status_changed`: resolved in the next release.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_next_release: bool,
    /// `status_changed`: the change came from a bulk request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bulk: bool,
    /// `regressed`: the release of the event that reopened the group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// `assigned`: the new assignee; `None` when unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<Assignee>,
    /// `assigned`: the assignee's name when the entry was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_name: Option<String>,
    /// `merged`: the groups merged in. `unmerged`, `split`: the group on the
    /// other side (both groups get an entry).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_ids: Vec<i32>,
    /// `merged`, `unmerged`, `split`: how many fingerprints moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprints: Option<u64>,
    /// `issue_linked`, `issue_unlinked`: the issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    /// `issue_linked`, `issue_unlinked`: its title when linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    /// `assigned`: the ownership rule pattern that assigned a new group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_rule: Option<String>,
}

/// One entry of a group's activity log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub id: i64,
    pub group_id: i32,
    pub kind: ActivityKind,
    pub actor_user_id: Option<i32>,
    pub actor_name: Option<String>,
    /// The comment text, for comments.
    pub body: Option<String>,
    pub details: ActivityDetails,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
}

/// A page of a group's activity, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityPage {
    pub entries: Vec<ActivityEntry>,
    pub total: u64,
}

fn validate_comment(body: &str) -> Result<&str, ErrorTrackingError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(ErrorTrackingError::Validation(
            "a comment cannot be empty".into(),
        ));
    }
    if body.chars().count() > MAX_COMMENT_CHARS {
        return Err(ErrorTrackingError::Validation(format!(
            "a comment is at most {MAX_COMMENT_CHARS} characters"
        )));
    }
    Ok(body)
}

fn details_json(details: &ActivityDetails) -> Result<serde_json::Value, ErrorTrackingError> {
    serde_json::to_value(details).map_err(|error| {
        ErrorTrackingError::Validation(format!("activity details do not serialize: {error}"))
    })
}

/// Record the same entry on many groups of one project in one statement.
pub(crate) async fn record_many<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    group_ids: &[i32],
    kind: ActivityKind,
    actor_user_id: Option<i32>,
    details: &ActivityDetails,
) -> Result<(), ErrorTrackingError> {
    if group_ids.is_empty() {
        return Ok(());
    }
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO error_group_activity (project_id, error_group_id, kind, actor_user_id, details) \
         SELECT $1, g.id, $3, $4, $5::jsonb FROM error_groups g \
         WHERE g.project_id = $1 AND g.id = ANY($2)",
        [
            project_id.into(),
            group_ids.to_vec().into(),
            kind.as_str().into(),
            actor_user_id.into(),
            details_json(details)?.into(),
        ],
    ))
    .await?;
    Ok(())
}

impl GroupWorkflowService {
    /// Record that something happened to groups of a project. Ids that are
    /// not groups of the project are skipped.
    pub async fn record_activity(
        &self,
        project_id: i32,
        group_ids: &[i32],
        kind: ActivityKind,
        actor_user_id: Option<i32>,
        details: ActivityDetails,
    ) -> Result<(), ErrorTrackingError> {
        record_many(
            self.db.as_ref(),
            project_id,
            group_ids,
            kind,
            actor_user_id,
            &details,
        )
        .await
    }

    /// Add a comment to a group.
    pub async fn add_comment(
        &self,
        project_id: i32,
        group_id: i32,
        author_user_id: i32,
        body: &str,
    ) -> Result<ActivityEntry, ErrorTrackingError> {
        let body = validate_comment(body)?;
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_group_activity (project_id, error_group_id, kind, actor_user_id, body) \
                 SELECT g.project_id, g.id, 'comment', $3, $4 FROM error_groups g \
                 WHERE g.id = $2 AND g.project_id = $1 RETURNING id",
                [
                    project_id.into(),
                    group_id.into(),
                    author_user_id.into(),
                    body.into(),
                ],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            })?;
        let id: i64 = row.try_get("", "id")?;
        self.activity_entry(project_id, group_id, id).await
    }

    /// Change a comment's text. Only its author may.
    pub async fn edit_comment(
        &self,
        project_id: i32,
        group_id: i32,
        comment_id: i64,
        editor_user_id: i32,
        body: &str,
    ) -> Result<ActivityEntry, ErrorTrackingError> {
        let body = validate_comment(body)?;
        let author = self
            .comment_author(project_id, group_id, comment_id)
            .await?;
        if author != Some(editor_user_id) {
            return Err(ErrorTrackingError::NotCommentAuthor {
                group_id,
                comment_id,
                action: "edited",
                admins: "",
            });
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_group_activity SET body = $2, edited_at = NOW() \
                 WHERE id = $1 AND kind = 'comment'",
                [comment_id.into(), body.into()],
            ))
            .await?;
        self.activity_entry(project_id, group_id, comment_id).await
    }

    /// Delete a comment. Its author may, and so may anyone `may_moderate`
    /// (the handler passes project administrators).
    pub async fn delete_comment(
        &self,
        project_id: i32,
        group_id: i32,
        comment_id: i64,
        user_id: i32,
        may_moderate: bool,
    ) -> Result<(), ErrorTrackingError> {
        let author = self
            .comment_author(project_id, group_id, comment_id)
            .await?;
        if author != Some(user_id) && !may_moderate {
            return Err(ErrorTrackingError::NotCommentAuthor {
                group_id,
                comment_id,
                action: "deleted",
                admins: " or an administrator",
            });
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM error_group_activity WHERE id = $1 AND kind = 'comment'",
                [comment_id.into()],
            ))
            .await?;
        Ok(())
    }

    /// A page of a group's activity, newest first.
    pub async fn list_activity(
        &self,
        project_id: i32,
        group_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<ActivityPage, ErrorTrackingError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let exists = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM error_group_activity WHERE error_group_id = g.id) AS total \
                 FROM error_groups g WHERE g.id = $1 AND g.project_id = $2",
                [group_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            })?;
        let total: i64 = exists.try_get("", "total")?;
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "{ENTRY_SELECT} WHERE a.error_group_id = $1 \
                     ORDER BY a.created_at DESC, a.id DESC LIMIT $2 OFFSET $3"
                ),
                [
                    group_id.into(),
                    (page_size as i64).into(),
                    (((page - 1) * page_size) as i64).into(),
                ],
            ))
            .await?;
        Ok(ActivityPage {
            entries: rows.iter().map(entry_from_row).collect::<Result<_, _>>()?,
            total: total.max(0) as u64,
        })
    }

    async fn comment_author(
        &self,
        project_id: i32,
        group_id: i32,
        comment_id: i64,
    ) -> Result<Option<i32>, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT actor_user_id FROM error_group_activity \
                 WHERE id = $1 AND error_group_id = $2 AND project_id = $3 AND kind = 'comment'",
                [comment_id.into(), group_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::CommentNotFound {
                group_id,
                comment_id,
            })?;
        Ok(row.try_get("", "actor_user_id")?)
    }

    async fn activity_entry(
        &self,
        project_id: i32,
        group_id: i32,
        id: i64,
    ) -> Result<ActivityEntry, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("{ENTRY_SELECT} WHERE a.id = $1 AND a.project_id = $2"),
                [id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::CommentNotFound {
                group_id,
                comment_id: id,
            })?;
        entry_from_row(&row)
    }
}

const ENTRY_SELECT: &str = "SELECT a.id, a.error_group_id, a.kind, a.actor_user_id, \
     u.name AS actor_name, a.body, a.details, a.created_at, a.edited_at \
     FROM error_group_activity a LEFT JOIN users u ON u.id = a.actor_user_id";

fn entry_from_row(row: &sea_orm::QueryResult) -> Result<ActivityEntry, ErrorTrackingError> {
    let kind: String = row.try_get("", "kind")?;
    let details: Option<serde_json::Value> = row.try_get("", "details")?;
    Ok(ActivityEntry {
        id: row.try_get("", "id")?,
        group_id: row.try_get("", "error_group_id")?,
        kind: ActivityKind::parse(&kind).ok_or_else(|| {
            ErrorTrackingError::Validation(format!("unknown activity kind '{kind}' in the log"))
        })?,
        actor_user_id: row.try_get("", "actor_user_id")?,
        actor_name: row.try_get("", "actor_name")?,
        body: row.try_get("", "body")?,
        // An entry written by a newer release with fields this one does not
        // know still reads; unknown fields are dropped.
        details: details
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        created_at: row.try_get("", "created_at")?,
        edited_at: row.try_get("", "edited_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_stored_name() {
        for kind in [
            ActivityKind::Comment,
            ActivityKind::StatusChanged,
            ActivityKind::Regressed,
            ActivityKind::Assigned,
            ActivityKind::Merged,
            ActivityKind::Unmerged,
            ActivityKind::Split,
        ] {
            assert_eq!(ActivityKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ActivityKind::parse("teleported"), None);
    }

    #[test]
    fn comments_are_trimmed_and_bounded() {
        assert_eq!(
            validate_comment("  looks like the cache  ").ok(),
            Some("looks like the cache")
        );
        assert!(validate_comment("   ").is_err());
        assert!(validate_comment(&"x".repeat(MAX_COMMENT_CHARS + 1)).is_err());
    }

    #[test]
    fn details_store_only_what_is_set() {
        let json = details_json(&ActivityDetails {
            status: Some("resolved".into()),
            ..Default::default()
        })
        .expect("serializes");
        assert_eq!(json, serde_json::json!({"status": "resolved"}));
    }
}
