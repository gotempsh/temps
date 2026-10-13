// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path routes (ADR 050): a service's hostnames can hand a path prefix to
//! another service of the same project, e.g. `frontend: /api → backend`, so
//! an app that calls a same-origin `/api` works without CORS or a second
//! domain.
//!
//! Routes are stored here and resolved by the proxy's route table when it
//! reloads (the table's trigger notifies it). Nothing in this module runs on
//! the request path.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
    SqlErr,
};
use serde::{Deserialize, Serialize};
use temps_entities::project_group_routes::{normalize_path_prefix, target_environment};
use temps_entities::{environments, project_group_routes, projects};
use utoipa::ToSchema;

use crate::error::ProjectGroupError;
use crate::service::ProjectGroupWithMembers;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateGroupRouteRequest {
    /// The service whose hostnames receive the requests.
    pub source_project_id: i32,
    /// Path prefix handed to the target, e.g. `/api`. Matches the prefix
    /// itself and everything below it (`/api/users`), never `/apiary`.
    #[schema(example = "/api")]
    pub path_prefix: String,
    /// The service that serves requests under the prefix.
    pub target_project_id: i32,
    /// Remove the prefix before forwarding (`/api/users` → `/users`). Off by
    /// default: most backends expect the path they were called with.
    #[serde(default)]
    pub strip_prefix: bool,
}

/// How a route applies to one environment of its source service.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupRouteEnvironment {
    pub source_environment_id: i32,
    #[schema(example = "production")]
    pub source_environment_slug: String,
    /// The target environment requests go to: the one with the same slug,
    /// or for a preview, the target's preview of the same branch. `None`
    /// means requests under the prefix are answered with an error instead of
    /// reaching another environment.
    pub target_environment_id: Option<i32>,
    pub target_environment_slug: Option<String>,
    /// Whether the target environment has a deployment to serve requests.
    pub target_deployed: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupRouteResponse {
    pub id: i32,
    pub group_id: i32,
    pub source_project_id: i32,
    pub source_project_name: String,
    #[schema(example = "/api")]
    pub path_prefix: String,
    pub target_project_id: i32,
    pub target_project_name: String,
    pub strip_prefix: bool,
    pub environments: Vec<GroupRouteEnvironment>,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct GroupRouteService {
    db: Arc<DatabaseConnection>,
}

impl GroupRouteService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn list(
        &self,
        group: &ProjectGroupWithMembers,
    ) -> Result<Vec<GroupRouteResponse>, ProjectGroupError> {
        let group_id = group.group.id;
        let routes = project_group_routes::Entity::find()
            .filter(project_group_routes::Column::GroupId.eq(group_id))
            .order_by_asc(project_group_routes::Column::SourceProjectId)
            .order_by_asc(project_group_routes::Column::PathPrefix)
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading routes of project group {group_id}"
            )))?;
        let environments = self.environments_of(&group.members).await?;
        Ok(routes
            .into_iter()
            .map(|route| describe(route, &group.members, &environments))
            .collect())
    }

    pub async fn create(
        &self,
        group: &ProjectGroupWithMembers,
        req: CreateGroupRouteRequest,
    ) -> Result<GroupRouteResponse, ProjectGroupError> {
        let group_id = group.group.id;
        let path_prefix = normalize_path_prefix(&req.path_prefix)
            .map_err(|message| ProjectGroupError::Validation { message })?;
        if req.source_project_id == req.target_project_id {
            return Err(ProjectGroupError::Validation {
                message: "A route must send requests to a different service".to_string(),
            });
        }
        let source = member(group, req.source_project_id)?;
        member(group, req.target_project_id)?;

        let now = chrono::Utc::now();
        let created = project_group_routes::ActiveModel {
            group_id: Set(group_id),
            source_project_id: Set(req.source_project_id),
            target_project_id: Set(req.target_project_id),
            path_prefix: Set(path_prefix.clone()),
            strip_prefix: Set(req.strip_prefix),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|e| match e.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => ProjectGroupError::RouteConflict {
                source_name: source.name.clone(),
                path_prefix: path_prefix.clone(),
            },
            _ => ProjectGroupError::db(format!("creating a route in project group {group_id}"))(e),
        })?;

        let environments = self.environments_of(&group.members).await?;
        Ok(describe(created, &group.members, &environments))
    }

    pub async fn delete(
        &self,
        route: &project_group_routes::Model,
    ) -> Result<(), ProjectGroupError> {
        project_group_routes::Entity::delete_by_id(route.id)
            .exec(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "deleting route {}",
                route.id
            )))?;
        Ok(())
    }

    /// Route row by id, scoped to its group, for authorization before delete.
    pub async fn find(
        &self,
        group_id: i32,
        route_id: i32,
    ) -> Result<project_group_routes::Model, ProjectGroupError> {
        project_group_routes::Entity::find_by_id(route_id)
            .filter(project_group_routes::Column::GroupId.eq(group_id))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!("loading route {route_id}")))?
            .ok_or(ProjectGroupError::RouteNotFound { group_id, route_id })
    }

    async fn environments_of(
        &self,
        members: &[projects::Model],
    ) -> Result<HashMap<i32, Vec<environments::Model>>, ProjectGroupError> {
        let ids: Vec<i32> = members.iter().map(|p| p.id).collect();
        let rows = environments::Entity::find()
            .filter(environments::Column::ProjectId.is_in(ids))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(
                "loading environments of the project's services",
            ))?;
        let mut by_project: HashMap<i32, Vec<environments::Model>> = HashMap::new();
        for env in rows {
            by_project.entry(env.project_id).or_default().push(env);
        }
        Ok(by_project)
    }
}

fn member(
    group: &ProjectGroupWithMembers,
    project_id: i32,
) -> Result<&projects::Model, ProjectGroupError> {
    group
        .members
        .iter()
        .find(|p| p.id == project_id)
        .ok_or(ProjectGroupError::NotAMember {
            group_id: group.group.id,
            project_id,
        })
}

fn describe(
    route: project_group_routes::Model,
    members: &[projects::Model],
    environments: &HashMap<i32, Vec<environments::Model>>,
) -> GroupRouteResponse {
    let name = |id: i32| {
        members
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    };
    let no_environments = Vec::new();
    let targets = environments
        .get(&route.target_project_id)
        .unwrap_or(&no_environments);
    let environments = environments
        .get(&route.source_project_id)
        .unwrap_or(&no_environments)
        .iter()
        .map(|source| {
            let target = target_environment(source, targets);
            GroupRouteEnvironment {
                source_environment_id: source.id,
                source_environment_slug: source.slug.clone(),
                target_environment_id: target.map(|t| t.id),
                target_environment_slug: target.map(|t| t.slug.clone()),
                target_deployed: target.is_some_and(|t| t.current_deployment_id.is_some()),
            }
        })
        .collect();
    GroupRouteResponse {
        id: route.id,
        group_id: route.group_id,
        source_project_id: route.source_project_id,
        source_project_name: name(route.source_project_id),
        path_prefix: route.path_prefix,
        target_project_id: route.target_project_id,
        target_project_name: name(route.target_project_id),
        strip_prefix: route.strip_prefix,
        environments,
        created_at: route.created_at,
    }
}

#[cfg(test)]
mod tests {
    // Database-backed: skips when no container runtime is available.
    use super::*;
    use crate::service::{CreateProjectGroupRequest, ProjectGroupService};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use temps_database::test_utils::TestDatabase;

    async fn insert_service(conn: &impl ConnectionTrait, id: i32) {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, preset,
                 created_at, updated_at, slug, is_deleted, is_public_repo, attack_mode,
                 enable_preview_environments, source_type)
               VALUES ($1, $2, 'r', 'o', '.', 'main', 'nextjs', NOW(), NOW(), $2, FALSE, TRUE,
                 FALSE, FALSE, 'git')"#,
            [id.into(), format!("svc-{id}").into()],
        ))
        .await
        .expect("insert project");
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, is_preview, created_at, updated_at)
               VALUES ('production', 'production', $1, $1, '[]'::json, $2, FALSE, NOW(), NOW())"#,
            [format!("svc-{id}-production").into(), id.into()],
        ))
        .await
        .expect("insert environment");
    }

    fn request(source: i32, prefix: &str, target: i32) -> CreateGroupRouteRequest {
        CreateGroupRouteRequest {
            source_project_id: source,
            path_prefix: prefix.to_string(),
            target_project_id: target,
            strip_prefix: false,
        }
    }

    #[tokio::test]
    async fn routes_validate_pair_environments_and_leave_with_their_services() {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        for id in [9801, 9802, 9803] {
            insert_service(conn.as_ref(), id).await;
        }
        let groups = ProjectGroupService::new(conn.clone());
        let created = groups
            .create_group(CreateProjectGroupRequest {
                name: "Shop".into(),
                slug: None,
                description: None,
                project_ids: vec![9801, 9802],
            })
            .await
            .unwrap();
        let group = groups.get_group(created.group.id).await.unwrap();
        let routes = GroupRouteService::new(conn.clone());

        let route = routes
            .create(&group, request(9801, "api/", 9802))
            .await
            .unwrap();
        assert_eq!(route.path_prefix, "/api");
        assert_eq!(route.target_project_name, "svc-9802");
        assert_eq!(route.environments.len(), 1);
        let env = &route.environments[0];
        assert_eq!(env.target_environment_slug.as_deref(), Some("production"));
        assert!(!env.target_deployed, "nothing is deployed in the fixture");

        assert!(matches!(
            routes.create(&group, request(9801, "/api", 9802)).await,
            Err(ProjectGroupError::RouteConflict { .. })
        ));
        assert!(matches!(
            routes.create(&group, request(9801, "/v2", 9801)).await,
            Err(ProjectGroupError::Validation { .. })
        ));
        assert!(matches!(
            routes
                .create(&group, request(9801, "/api/_temps", 9802))
                .await,
            Err(ProjectGroupError::Validation { .. })
        ));
        assert!(matches!(
            routes.create(&group, request(9801, "/jobs", 9803)).await,
            Err(ProjectGroupError::NotAMember { .. })
        ));

        // Leaving the group takes the service's routes with it, whatever
        // removed it.
        groups.remove_project(group.group.id, 9802).await.unwrap();
        let group = groups.get_group(group.group.id).await.unwrap();
        assert!(routes.list(&group).await.unwrap().is_empty());
    }
}
