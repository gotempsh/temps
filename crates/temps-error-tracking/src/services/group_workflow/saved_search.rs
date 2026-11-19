// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved searches over error groups (ADR 111, D1.8): a named set of list
//! filters, owned by one user in one project, private unless shared.
//!
//! An `assignee` of `me` is stored as written and resolved for whoever opens
//! the search, so a shared "assigned to me" search means each viewer's own
//! groups.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::super::types::ErrorTrackingError;
use super::assignee::AssigneeFilter;
use super::{GroupWorkflowService, BULK_SEARCH_MAX_CHARS};

/// Longest saved search name, in characters.
pub const MAX_SAVED_SEARCH_NAME_CHARS: usize = 100;
/// Saved searches one user may hold in one project.
pub const MAX_SAVED_SEARCHES_PER_USER: u64 = 50;

const STATUSES: [&str; 3] = ["unresolved", "resolved", "ignored"];
const SORTS: [&str; 4] = ["last_seen", "first_seen", "total_count", "events_in_range"];

/// The list filters a saved search applies. Every field is optional; an
/// empty set is "all groups, newest first".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SavedSearchFilters {
    /// `unresolved`, `resolved` or `ignored`; omitted for every status.
    #[serde(default)]
    pub status: Option<String>,
    /// An environment of the project.
    #[serde(default)]
    pub environment_id: Option<i32>,
    /// `me`, `none`, `user:<id>` or `team:<id>`.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Text in the title, error type or message.
    #[serde(default)]
    pub search: Option<String>,
    /// `last_seen` (default), `first_seen`, `total_count` or `events_in_range`.
    #[serde(default)]
    pub sort_by: Option<String>,
    /// `asc` or `desc` (default).
    #[serde(default)]
    pub sort_order: Option<String>,
}

impl SavedSearchFilters {
    /// Check every field and normalize it: trimmed, empty strings dropped,
    /// lower-case keywords.
    pub fn validated(self) -> Result<Self, ErrorTrackingError> {
        fn clean(value: Option<String>) -> Option<String> {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }
        let status = clean(self.status).map(|status| status.to_ascii_lowercase());
        if let Some(status) = status.as_deref() {
            if !STATUSES.contains(&status) {
                return Err(ErrorTrackingError::Validation(format!(
                    "saved search status '{status}' must be one of {}",
                    STATUSES.join(", ")
                )));
            }
        }
        let assignee = clean(self.assignee);
        if let Some(assignee) = assignee.as_deref() {
            // Any user id parses the same way; only the shape is checked here.
            AssigneeFilter::parse(assignee, 0)?;
        }
        let search = clean(self.search);
        if search
            .as_deref()
            .is_some_and(|search| search.chars().count() > BULK_SEARCH_MAX_CHARS)
        {
            return Err(ErrorTrackingError::Validation(format!(
                "saved search text is at most {BULK_SEARCH_MAX_CHARS} characters"
            )));
        }
        let sort_by = clean(self.sort_by).map(|sort| sort.to_ascii_lowercase());
        if let Some(sort) = sort_by.as_deref() {
            if !SORTS.contains(&sort) {
                return Err(ErrorTrackingError::Validation(format!(
                    "saved search sort '{sort}' must be one of {}",
                    SORTS.join(", ")
                )));
            }
        }
        let sort_order = clean(self.sort_order).map(|order| order.to_ascii_lowercase());
        if let Some(order) = sort_order.as_deref() {
            if order != "asc" && order != "desc" {
                return Err(ErrorTrackingError::Validation(format!(
                    "saved search sort order '{order}' must be asc or desc"
                )));
            }
        }
        Ok(Self {
            status,
            environment_id: self.environment_id,
            assignee,
            search,
            sort_by,
            sort_order,
        })
    }
}

/// A saved search as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSearch {
    pub id: i32,
    pub project_id: i32,
    pub owner_user_id: i32,
    pub owner_name: Option<String>,
    pub name: String,
    pub shared: bool,
    pub filters: SavedSearchFilters,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A page of the saved searches a user can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSearchPage {
    pub searches: Vec<SavedSearch>,
    pub total: u64,
}

/// What a change to a saved search sets; `None` keeps the current value.
#[derive(Debug, Clone, Default)]
pub struct SavedSearchChange {
    pub name: Option<String>,
    pub shared: Option<bool>,
    pub filters: Option<SavedSearchFilters>,
}

fn validate_name(name: &str) -> Result<String, ErrorTrackingError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ErrorTrackingError::Validation(
            "a saved search needs a name".into(),
        ));
    }
    if name.chars().count() > MAX_SAVED_SEARCH_NAME_CHARS {
        return Err(ErrorTrackingError::Validation(format!(
            "a saved search name is at most {MAX_SAVED_SEARCH_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_string())
}

/// A unique-index violation on the owner's name.
fn is_duplicate_name(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

const SEARCH_SELECT: &str = "SELECT s.id, s.project_id, s.owner_user_id, u.name AS owner_name, \
     s.name, s.shared, s.status, s.environment_id, s.assignee, s.search, s.sort_by, \
     s.sort_order, s.created_at, s.updated_at \
     FROM error_saved_searches s LEFT JOIN users u ON u.id = s.owner_user_id";

fn search_from_row(row: &sea_orm::QueryResult) -> Result<SavedSearch, ErrorTrackingError> {
    Ok(SavedSearch {
        id: row.try_get("", "id")?,
        project_id: row.try_get("", "project_id")?,
        owner_user_id: row.try_get("", "owner_user_id")?,
        owner_name: row.try_get("", "owner_name")?,
        name: row.try_get("", "name")?,
        shared: row.try_get("", "shared")?,
        filters: SavedSearchFilters {
            status: row.try_get("", "status")?,
            environment_id: row.try_get("", "environment_id")?,
            assignee: row.try_get("", "assignee")?,
            search: row.try_get("", "search")?,
            sort_by: row.try_get("", "sort_by")?,
            sort_order: Some(row.try_get("", "sort_order")?),
        },
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}

impl GroupWorkflowService {
    /// The caller's saved searches in a project and the ones others shared,
    /// newest first.
    pub async fn list_saved_searches(
        &self,
        project_id: i32,
        viewer_user_id: Option<i32>,
        page: u64,
        page_size: u64,
    ) -> Result<SavedSearchPage, ErrorTrackingError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let visible = "s.project_id = $1 AND (s.shared OR s.owner_user_id = $2)";
        let total: i64 = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("SELECT COUNT(*) AS total FROM error_saved_searches s WHERE {visible}"),
                [project_id.into(), viewer_user_id.into()],
            ))
            .await?
            .map(|row| row.try_get("", "total"))
            .transpose()?
            .unwrap_or(0);
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "{SEARCH_SELECT} WHERE {visible} \
                     ORDER BY s.created_at DESC, s.id DESC LIMIT $3 OFFSET $4"
                ),
                [
                    project_id.into(),
                    viewer_user_id.into(),
                    (page_size as i64).into(),
                    (((page - 1) * page_size) as i64).into(),
                ],
            ))
            .await?;
        Ok(SavedSearchPage {
            searches: rows.iter().map(search_from_row).collect::<Result<_, _>>()?,
            total: total.max(0) as u64,
        })
    }

    /// One saved search the viewer can see: theirs, or a shared one.
    pub async fn get_saved_search(
        &self,
        project_id: i32,
        saved_search_id: i32,
        viewer_user_id: Option<i32>,
    ) -> Result<SavedSearch, ErrorTrackingError> {
        let search = self.saved_search(project_id, saved_search_id).await?;
        if !search.shared && Some(search.owner_user_id) != viewer_user_id {
            // Someone else's private search does not exist for the viewer.
            return Err(ErrorTrackingError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            });
        }
        Ok(search)
    }

    /// Save a search for `owner_user_id`.
    pub async fn create_saved_search(
        &self,
        project_id: i32,
        owner_user_id: i32,
        name: &str,
        shared: bool,
        filters: SavedSearchFilters,
    ) -> Result<SavedSearch, ErrorTrackingError> {
        let name = validate_name(name)?;
        let filters = filters.validated()?;
        self.check_environment(project_id, filters.environment_id)
            .await?;
        // A per-owner lock for the transaction, so two concurrent requests
        // cannot both see room for one more and both insert.
        let tx = self.db.begin().await?;
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('error_saved_searches'), $1)",
            [owner_user_id.into()],
        ))
        .await?;
        let row = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO error_saved_searches (project_id, owner_user_id, name, shared, status, \
                     environment_id, assignee, search, sort_by, sort_order) \
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10 \
                 WHERE (SELECT COUNT(*) FROM error_saved_searches \
                        WHERE project_id = $1 AND owner_user_id = $2) < $11 \
                 RETURNING id",
                [
                    project_id.into(),
                    owner_user_id.into(),
                    name.clone().into(),
                    shared.into(),
                    filters.status.into(),
                    filters.environment_id.into(),
                    filters.assignee.into(),
                    filters.search.into(),
                    filters.sort_by.into(),
                    filters.sort_order.unwrap_or_else(|| "desc".into()).into(),
                    (MAX_SAVED_SEARCHES_PER_USER as i64).into(),
                ],
            ))
            .await
            .map_err(|error| {
                if is_duplicate_name(&error) {
                    ErrorTrackingError::SavedSearchExists {
                        project_id,
                        name: name.clone(),
                    }
                } else {
                    ErrorTrackingError::Database(error)
                }
            })?
            .ok_or(ErrorTrackingError::SavedSearchLimit {
                project_id,
                user_id: owner_user_id,
                limit: MAX_SAVED_SEARCHES_PER_USER,
            })?;
        let id: i32 = row.try_get("", "id")?;
        tx.commit().await?;
        self.saved_search(project_id, id).await
    }

    /// Rename, share or unshare, or change the filters of a saved search.
    /// Only its owner may.
    pub async fn update_saved_search(
        &self,
        project_id: i32,
        saved_search_id: i32,
        editor_user_id: i32,
        change: SavedSearchChange,
    ) -> Result<SavedSearch, ErrorTrackingError> {
        let current = self
            .get_saved_search(project_id, saved_search_id, Some(editor_user_id))
            .await?;
        if current.owner_user_id != editor_user_id {
            return Err(ErrorTrackingError::NotSavedSearchOwner {
                project_id,
                saved_search_id,
                action: "changed",
                admins: "",
            });
        }
        let name = match change.name {
            Some(name) => validate_name(&name)?,
            None => current.name,
        };
        let filters = match change.filters {
            Some(filters) => filters.validated()?,
            None => current.filters,
        };
        self.check_environment(project_id, filters.environment_id)
            .await?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_saved_searches SET name = $3, shared = $4, status = $5, \
                     environment_id = $6, assignee = $7, search = $8, sort_by = $9, \
                     sort_order = $10, updated_at = NOW() \
                 WHERE id = $1 AND project_id = $2",
                [
                    saved_search_id.into(),
                    project_id.into(),
                    name.clone().into(),
                    change.shared.unwrap_or(current.shared).into(),
                    filters.status.into(),
                    filters.environment_id.into(),
                    filters.assignee.into(),
                    filters.search.into(),
                    filters.sort_by.into(),
                    filters.sort_order.unwrap_or_else(|| "desc".into()).into(),
                ],
            ))
            .await
            .map_err(|error| {
                if is_duplicate_name(&error) {
                    ErrorTrackingError::SavedSearchExists {
                        project_id,
                        name: name.clone(),
                    }
                } else {
                    ErrorTrackingError::Database(error)
                }
            })?;
        self.saved_search(project_id, saved_search_id).await
    }

    /// Delete a saved search. Its owner may; an administrator (`may_moderate`)
    /// may delete a shared one.
    pub async fn delete_saved_search(
        &self,
        project_id: i32,
        saved_search_id: i32,
        user_id: Option<i32>,
        may_moderate: bool,
    ) -> Result<(), ErrorTrackingError> {
        let current = self
            .get_saved_search(project_id, saved_search_id, user_id)
            .await?;
        if Some(current.owner_user_id) != user_id && !may_moderate {
            return Err(ErrorTrackingError::NotSavedSearchOwner {
                project_id,
                saved_search_id,
                action: "deleted",
                admins: " or an administrator",
            });
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM error_saved_searches WHERE id = $1 AND project_id = $2",
                [saved_search_id.into(), project_id.into()],
            ))
            .await?;
        Ok(())
    }

    async fn saved_search(
        &self,
        project_id: i32,
        saved_search_id: i32,
    ) -> Result<SavedSearch, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("{SEARCH_SELECT} WHERE s.id = $1 AND s.project_id = $2"),
                [saved_search_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            })?;
        search_from_row(&row)
    }

    async fn check_environment(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(), ErrorTrackingError> {
        let Some(environment_id) = environment_id else {
            return Ok(());
        };
        let found = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT 1 AS found FROM environments WHERE id = $1 AND project_id = $2",
                [environment_id.into(), project_id.into()],
            ))
            .await?;
        if found.is_none() {
            return Err(ErrorTrackingError::Validation(format!(
                "environment {environment_id} is not an environment of project {project_id}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_trimmed_lowercased_and_checked() {
        let filters = SavedSearchFilters {
            status: Some(" Unresolved ".into()),
            assignee: Some(" me ".into()),
            search: Some("   ".into()),
            sort_by: Some("TOTAL_COUNT".into()),
            sort_order: Some("ASC".into()),
            environment_id: Some(4),
        }
        .validated()
        .expect("valid");
        assert_eq!(filters.status.as_deref(), Some("unresolved"));
        assert_eq!(filters.assignee.as_deref(), Some("me"));
        assert_eq!(filters.search, None, "blank text is no filter");
        assert_eq!(filters.sort_by.as_deref(), Some("total_count"));
        assert_eq!(filters.sort_order.as_deref(), Some("asc"));
    }

    #[test]
    fn unknown_values_are_refused_with_the_allowed_ones() {
        for (filters, expected) in [
            (
                SavedSearchFilters {
                    status: Some("assigned".into()),
                    ..Default::default()
                },
                "unresolved, resolved, ignored",
            ),
            (
                SavedSearchFilters {
                    assignee: Some("someone".into()),
                    ..Default::default()
                },
                "me, none, user:<id> or team:<id>",
            ),
            (
                SavedSearchFilters {
                    sort_by: Some("title".into()),
                    ..Default::default()
                },
                "last_seen",
            ),
            (
                SavedSearchFilters {
                    sort_order: Some("up".into()),
                    ..Default::default()
                },
                "asc or desc",
            ),
            (
                SavedSearchFilters {
                    search: Some("x".repeat(BULK_SEARCH_MAX_CHARS + 1)),
                    ..Default::default()
                },
                "at most",
            ),
        ] {
            let error = filters.validated().expect_err("invalid");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(
            validate_name("  Mine, open  ").ok().as_deref(),
            Some("Mine, open")
        );
        assert!(validate_name(" ").is_err());
        assert!(validate_name(&"n".repeat(MAX_SAVED_SEARCH_NAME_CHARS + 1)).is_err());
    }
}
