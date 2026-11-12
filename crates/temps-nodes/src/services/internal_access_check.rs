// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a project may call an internal name (ADR 101 D7), for the DNS
//! *Test resolution* tool.
//!
//! A name resolving is half of a working call: the internal proxy then
//! answers 403 to a caller from another project unless the name's project
//! granted it internal access. Asked as a project, the test reports which
//! project owns the name and whether a grant admits the caller, so the console
//! can link to the owner's Internal access settings instead of leaving the
//! operator to discover the 403 from the app's logs.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, FromQueryResult, Statement};
use temps_core::internal_access::{
    callers_for_route, InternalAccessCaller, InternalAccessGrantScope,
};

/// How far a grant on the name's project reaches the asking project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalAccessReach {
    /// The name belongs to the asking project: no grant needed.
    SameProject,
    /// Every environment of the asking project may call it.
    Granted,
    /// Only some environments of the asking project may call it.
    SomeEnvironments,
    /// The internal proxy answers 403 to the asking project.
    NotGranted,
}

impl InternalAccessReach {
    pub fn as_str(self) -> &'static str {
        match self {
            InternalAccessReach::SameProject => "same_project",
            InternalAccessReach::Granted => "granted",
            InternalAccessReach::SomeEnvironments => "some_environments",
            InternalAccessReach::NotGranted => "not_granted",
        }
    }
}

/// Who owns an internal name, and whether the asking project may call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalAccessVerdict {
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    pub target_environment_id: i32,
    pub target_environment_name: String,
    pub reach: InternalAccessReach,
}

/// `<environment>.<project>.temps.local`, the only internal-route form, as
/// `(environment slug, project slug)`.
pub fn internal_route_labels(name: &str) -> Option<(String, String)> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let labels = name.strip_suffix(".temps.local")?;
    let (environment, project) = labels.split_once('.')?;
    if environment.is_empty() || project.is_empty() || project.contains('.') {
        return None;
    }
    Some((environment.to_string(), project.to_string()))
}

/// The pure part: what `grants` (all on `target_project_id`) give
/// `caller_project_id` on `target_environment_id`.
pub fn reach(
    grants: &[InternalAccessGrantScope],
    target_project_id: i32,
    target_environment_id: i32,
    caller_project_id: i32,
) -> InternalAccessReach {
    if target_project_id == caller_project_id {
        return InternalAccessReach::SameProject;
    }
    let callers: Vec<InternalAccessCaller> =
        callers_for_route(grants, target_project_id, Some(target_environment_id))
            .into_iter()
            .filter(|caller| caller.project_id == caller_project_id)
            .collect();
    if callers.iter().any(|caller| caller.environment_id.is_none()) {
        InternalAccessReach::Granted
    } else if callers.is_empty() {
        InternalAccessReach::NotGranted
    } else {
        InternalAccessReach::SomeEnvironments
    }
}

#[derive(Debug, FromQueryResult)]
struct TargetRow {
    project_id: i32,
    project_name: String,
    project_slug: String,
    environment_id: i32,
    environment_name: String,
}

#[derive(Debug, FromQueryResult)]
struct GrantRow {
    target_environment_id: Option<i32>,
    caller_environment_id: Option<i32>,
}

/// The verdict for `name` asked as `caller_project_id`, or `None` when the
/// name is not a live environment's internal route.
pub async fn internal_access_verdict<C: ConnectionTrait>(
    db: &C,
    name: &str,
    caller_project_id: i32,
) -> Result<Option<InternalAccessVerdict>, DbErr> {
    let Some((environment_slug, project_slug)) = internal_route_labels(name) else {
        return Ok(None);
    };
    let Some(target) = TargetRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT p.id AS project_id, p.name AS project_name, p.slug AS project_slug, \
                e.id AS environment_id, e.name AS environment_name \
         FROM environments e JOIN projects p ON p.id = e.project_id \
         WHERE p.slug = $1 AND e.slug = $2 AND NOT p.is_deleted AND e.deleted_at IS NULL \
         LIMIT 1",
        [project_slug.into(), environment_slug.into()],
    ))
    .one(db)
    .await?
    else {
        return Ok(None);
    };
    let grants = if target.project_id == caller_project_id {
        Vec::new()
    } else {
        GrantRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT target_environment_id, caller_environment_id \
             FROM internal_access_grants \
             WHERE target_project_id = $1 AND caller_project_id = $2",
            [target.project_id.into(), caller_project_id.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|row| InternalAccessGrantScope {
            target_project_id: target.project_id,
            target_environment_id: row.target_environment_id,
            caller: InternalAccessCaller {
                project_id: caller_project_id,
                environment_id: row.caller_environment_id,
            },
        })
        .collect()
    };
    Ok(Some(InternalAccessVerdict {
        reach: reach(
            &grants,
            target.project_id,
            target.environment_id,
            caller_project_id,
        ),
        target_project_id: target.project_id,
        target_project_name: target.project_name,
        target_project_slug: target.project_slug,
        target_environment_id: target.environment_id,
        target_environment_name: target.environment_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(target_env: Option<i32>, caller_env: Option<i32>) -> InternalAccessGrantScope {
        InternalAccessGrantScope {
            target_project_id: 1,
            target_environment_id: target_env,
            caller: InternalAccessCaller {
                project_id: 2,
                environment_id: caller_env,
            },
        }
    }

    #[test]
    fn only_environment_project_names_in_the_internal_zone_are_routes() {
        assert_eq!(
            internal_route_labels("Production.Orders-API.temps.local."),
            Some(("production".to_string(), "orders-api".to_string()))
        );
        assert_eq!(internal_route_labels("orders-api.temps.local"), None);
        assert_eq!(internal_route_labels("pg-0.pg.orders.temps.local"), None);
        assert_eq!(internal_route_labels("production.orders.example.com"), None);
        assert_eq!(internal_route_labels(".orders.temps.local"), None);
    }

    #[test]
    fn reach_follows_the_grants_scopes() {
        assert_eq!(reach(&[], 1, 10, 1), InternalAccessReach::SameProject);
        assert_eq!(reach(&[], 1, 10, 2), InternalAccessReach::NotGranted);
        assert_eq!(
            reach(&[grant(None, None)], 1, 10, 2),
            InternalAccessReach::Granted
        );
        assert_eq!(
            reach(&[grant(Some(10), Some(20))], 1, 10, 2),
            InternalAccessReach::SomeEnvironments
        );
        assert_eq!(
            reach(&[grant(Some(11), None)], 1, 10, 2),
            InternalAccessReach::NotGranted,
            "a grant on another environment of the target"
        );
    }
}
