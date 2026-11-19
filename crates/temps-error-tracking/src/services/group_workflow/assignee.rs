// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Assignment of an error group to a user or a team (ADR 111, D1.6). The
//! assignee is independent of the status: an assigned group can be resolved
//! or ignored and keeps its assignee.

use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::super::types::ErrorTrackingError;
use super::GroupWorkflowService;

/// Most team members notified of one assignment.
pub const MAX_ASSIGNMENT_RECIPIENTS: usize = 50;

/// Most candidates one assignee search returns, per kind.
pub const MAX_ASSIGNEE_OPTIONS: u64 = 20;

/// Longest assignee search text.
pub const MAX_ASSIGNEE_QUERY_CHARS: usize = 100;

/// Who a group is assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Assignee {
    User(i32),
    Team(i32),
}

impl Assignee {
    fn columns(assignee: Option<Self>) -> (Option<i32>, Option<i32>) {
        match assignee {
            Some(Self::User(id)) => (Some(id), None),
            Some(Self::Team(id)) => (None, Some(id)),
            None => (None, None),
        }
    }

    /// The assignee stored in a group's columns.
    pub fn of_group(group: &temps_entities::error_groups::Model) -> Option<Self> {
        match (group.assignee_user_id, group.assignee_team_id) {
            (Some(id), _) => Some(Self::User(id)),
            (None, Some(id)) => Some(Self::Team(id)),
            (None, None) => None,
        }
    }
}

/// An assignee with what a reader needs to show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GroupAssignee {
    /// `user` or `team`.
    #[serde(rename = "type")]
    pub kind: String,
    pub id: i32,
    /// The user's or team's name.
    pub name: String,
    /// The user's email; `None` for a team.
    pub email: Option<String>,
}

/// Which groups a list shows by assignee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssigneeFilter {
    /// Assigned to this user, or to a team this user is a member of.
    Me(i32),
    User(i32),
    Team(i32),
    /// Assigned to nobody.
    Unassigned,
}

impl AssigneeFilter {
    /// Parse the `assignee` query value: `me`, `none`, `user:<id>` or
    /// `team:<id>`. `me` needs the caller's user id.
    pub fn parse(value: &str, current_user_id: i32) -> Result<Self, ErrorTrackingError> {
        let value = value.trim();
        let invalid = || {
            ErrorTrackingError::Validation(format!(
                "assignee filter '{value}' must be me, none, user:<id> or team:<id>"
            ))
        };
        match value {
            "me" => Ok(Self::Me(current_user_id)),
            "none" => Ok(Self::Unassigned),
            _ => {
                let (kind, id) = value.split_once(':').ok_or_else(invalid)?;
                let id: i32 = id.parse().map_err(|_| invalid())?;
                match kind {
                    "user" => Ok(Self::User(id)),
                    "team" => Ok(Self::Team(id)),
                    _ => Err(invalid()),
                }
            }
        }
    }

    /// The Sea-ORM condition selecting the groups this filter shows.
    pub fn condition(&self) -> sea_orm::Condition {
        use sea_orm::sea_query::{Expr, Query};
        use sea_orm::ColumnTrait;
        use temps_entities::error_groups::Column;
        match *self {
            Self::Me(user_id) => sea_orm::Condition::any()
                .add(Column::AssigneeUserId.eq(user_id))
                .add(
                    Column::AssigneeTeamId.in_subquery(
                        Query::select()
                            .column(temps_entities::team_members::Column::TeamId)
                            .from(temps_entities::team_members::Entity)
                            .and_where(
                                Expr::col(temps_entities::team_members::Column::UserId).eq(user_id),
                            )
                            .to_owned(),
                    ),
                ),
            Self::User(id) => sea_orm::Condition::all().add(Column::AssigneeUserId.eq(id)),
            Self::Team(id) => sea_orm::Condition::all().add(Column::AssigneeTeamId.eq(id)),
            Self::Unassigned => sea_orm::Condition::all()
                .add(Column::AssigneeUserId.is_null())
                .add(Column::AssigneeTeamId.is_null()),
        }
    }
}

/// What an assignment changed, and who to tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentOutcome {
    pub previous: Option<Assignee>,
    pub current: Option<GroupAssignee>,
    /// Emails of the new assignee (the user, or up to
    /// [`MAX_ASSIGNMENT_RECIPIENTS`] team members). Empty when nothing
    /// changed or the group was unassigned.
    pub recipients: Vec<String>,
    pub group_title: String,
}

/// Candidates for an assignment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssigneeCandidates {
    pub users: Vec<GroupAssignee>,
    pub teams: Vec<GroupAssignee>,
}

impl GroupWorkflowService {
    /// Assign a group to a user or a team, or unassign it with `None`. The
    /// status is not touched. The caller checks that a user assignee can
    /// access the project, then audit-logs and notifies.
    pub async fn set_assignee(
        &self,
        project_id: i32,
        group_id: i32,
        assignee: Option<Assignee>,
    ) -> Result<AssignmentOutcome, ErrorTrackingError> {
        let tx = self.db.begin().await?;
        let current = match assignee {
            Some(assignee) => Some(describe(&tx, assignee).await?),
            None => None,
        };
        let row = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT title, assignee_user_id, assignee_team_id FROM error_groups \
                 WHERE id = $1 AND project_id = $2 FOR UPDATE",
                [group_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            })?;
        let previous = match (
            row.try_get::<Option<i32>>("", "assignee_user_id")?,
            row.try_get::<Option<i32>>("", "assignee_team_id")?,
        ) {
            (Some(id), _) => Some(Assignee::User(id)),
            (None, Some(id)) => Some(Assignee::Team(id)),
            (None, None) => None,
        };
        let group_title: String = row.try_get("", "title")?;
        let (user_id, team_id) = Assignee::columns(assignee);
        // Writing the assignee also retires the legacy free-text value.
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET assignee_user_id = $3, assignee_team_id = $4, \
             assigned_to = NULL, updated_at = NOW() WHERE id = $1 AND project_id = $2",
            [
                group_id.into(),
                project_id.into(),
                user_id.into(),
                team_id.into(),
            ],
        ))
        .await?;
        let recipients = match assignee {
            Some(assignee) if previous != Some(assignee) => recipients(&tx, assignee).await?,
            _ => Vec::new(),
        };
        tx.commit().await?;
        Ok(AssignmentOutcome {
            previous,
            current,
            recipients,
            group_title,
        })
    }

    /// The user whose email is `email`, for the legacy `assigned_to` field.
    pub async fn user_by_email(&self, email: &str) -> Result<Assignee, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM users WHERE lower(email) = lower($1) AND deleted_at IS NULL \
                 ORDER BY id LIMIT 1",
                [email.trim().into()],
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation(format!(
                    "assigned_to '{}' is not the email of a user; assign a user or team by id",
                    email.trim()
                ))
            })?;
        Ok(Assignee::User(row.try_get("", "id")?))
    }

    /// Users and teams whose name (or a user's email) contains `query`, at
    /// most [`MAX_ASSIGNEE_OPTIONS`] of each. The caller keeps only users who
    /// can access the project.
    pub async fn search_assignees(
        &self,
        query: &str,
    ) -> Result<AssigneeCandidates, ErrorTrackingError> {
        if query.chars().count() > MAX_ASSIGNEE_QUERY_CHARS {
            return Err(ErrorTrackingError::Validation(format!(
                "assignee search is longer than {MAX_ASSIGNEE_QUERY_CHARS} characters"
            )));
        }
        let pattern = super::like_pattern(query);
        let users = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, name, email FROM users WHERE deleted_at IS NULL \
                 AND (name ILIKE $1 OR email ILIKE $1) ORDER BY name, id LIMIT $2",
                [pattern.clone().into(), (MAX_ASSIGNEE_OPTIONS as i64).into()],
            ))
            .await?
            .into_iter()
            .map(|row| {
                Ok(GroupAssignee {
                    kind: "user".into(),
                    id: row.try_get("", "id")?,
                    name: row.try_get("", "name")?,
                    email: Some(row.try_get("", "email")?),
                })
            })
            .collect::<Result<Vec<_>, ErrorTrackingError>>()?;
        let teams = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, name FROM teams WHERE name ILIKE $1 ORDER BY name, id LIMIT $2",
                [pattern.into(), (MAX_ASSIGNEE_OPTIONS as i64).into()],
            ))
            .await?
            .into_iter()
            .map(|row| {
                Ok(GroupAssignee {
                    kind: "team".into(),
                    id: row.try_get("", "id")?,
                    name: row.try_get("", "name")?,
                    email: None,
                })
            })
            .collect::<Result<Vec<_>, ErrorTrackingError>>()?;
        Ok(AssigneeCandidates { users, teams })
    }
}

/// Load the name of an assignee, failing when it does not exist.
async fn describe<C: ConnectionTrait>(
    db: &C,
    assignee: Assignee,
) -> Result<GroupAssignee, ErrorTrackingError> {
    let found = load_assignees(db, &[assignee]).await?;
    found.get(&assignee).cloned().ok_or_else(|| match assignee {
        Assignee::User(id) => {
            ErrorTrackingError::Validation(format!("user {id} does not exist or has been deleted"))
        }
        Assignee::Team(id) => ErrorTrackingError::Validation(format!("team {id} does not exist")),
    })
}

/// Emails to notify of a new assignment.
async fn recipients<C: ConnectionTrait>(
    db: &C,
    assignee: Assignee,
) -> Result<Vec<String>, ErrorTrackingError> {
    let statement = match assignee {
        Assignee::User(id) => Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT email FROM users WHERE id = $1 AND deleted_at IS NULL",
            [id.into()],
        ),
        Assignee::Team(id) => Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT u.email FROM team_members m JOIN users u ON u.id = m.user_id \
             WHERE m.team_id = $1 AND u.deleted_at IS NULL ORDER BY u.id LIMIT $2",
            [id.into(), (MAX_ASSIGNMENT_RECIPIENTS as i64).into()],
        ),
    };
    db.query_all(statement)
        .await?
        .into_iter()
        .map(|row| Ok(row.try_get("", "email")?))
        .filter(|email: &Result<String, ErrorTrackingError>| {
            email.as_ref().map_or(true, |e| !e.trim().is_empty())
        })
        .collect()
}

/// Names of many assignees in at most two queries.
pub(crate) async fn load_assignees<C: ConnectionTrait>(
    db: &C,
    assignees: &[Assignee],
) -> Result<HashMap<Assignee, GroupAssignee>, ErrorTrackingError> {
    let mut user_ids = Vec::new();
    let mut team_ids = Vec::new();
    for assignee in assignees {
        match *assignee {
            Assignee::User(id) => user_ids.push(id),
            Assignee::Team(id) => team_ids.push(id),
        }
    }
    user_ids.sort_unstable();
    user_ids.dedup();
    team_ids.sort_unstable();
    team_ids.dedup();
    let mut found = HashMap::new();
    if !user_ids.is_empty() {
        for row in db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, name, email FROM users WHERE id = ANY($1) AND deleted_at IS NULL",
                [user_ids.into()],
            ))
            .await?
        {
            let id: i32 = row.try_get("", "id")?;
            found.insert(
                Assignee::User(id),
                GroupAssignee {
                    kind: "user".into(),
                    id,
                    name: row.try_get("", "name")?,
                    email: Some(row.try_get("", "email")?),
                },
            );
        }
    }
    if !team_ids.is_empty() {
        for row in db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, name FROM teams WHERE id = ANY($1)",
                [team_ids.into()],
            ))
            .await?
        {
            let id: i32 = row.try_get("", "id")?;
            found.insert(
                Assignee::Team(id),
                GroupAssignee {
                    kind: "team".into(),
                    id,
                    name: row.try_get("", "name")?,
                    email: None,
                },
            );
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assignee_filter_parses_every_form_and_refuses_others() {
        assert_eq!(
            AssigneeFilter::parse("me", 7).ok(),
            Some(AssigneeFilter::Me(7))
        );
        assert_eq!(
            AssigneeFilter::parse("none", 7).ok(),
            Some(AssigneeFilter::Unassigned)
        );
        assert_eq!(
            AssigneeFilter::parse("user:3", 7).ok(),
            Some(AssigneeFilter::User(3))
        );
        assert_eq!(
            AssigneeFilter::parse(" team:12 ", 7).ok(),
            Some(AssigneeFilter::Team(12))
        );
        for bad in ["", "you", "user:", "user:x", "group:1", "team:1:2"] {
            assert!(
                AssigneeFilter::parse(bad, 7).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn me_matches_the_user_or_one_of_their_teams() {
        use sea_orm::{EntityTrait, QueryFilter, QueryTrait};
        let sql = temps_entities::error_groups::Entity::find()
            .filter(AssigneeFilter::Me(5).condition())
            .build(sea_orm::DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""assignee_user_id" = 5"#), "{sql}");
        assert!(sql.contains(r#""team_members""#), "{sql}");
    }
}
