// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use std::collections::BTreeSet;
use temps_authz::permissions::{Permission, Role};

impl PolicyPlanner {
    pub(super) async fn recheck(
        &self,
        boot_id: Uuid,
        grants: &[Candidate],
    ) -> Result<BTreeMap<(i32, i32), bool>, PlanError> {
        let checker = self.checker.0.get().ok_or_else(|| {
            invalid(
                boot_id,
                "recheck project permissions",
                "project access checker has not been initialized",
            )
        })?;
        let mut projects_by_user: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for grant in grants.iter().filter(|g| !g.admin) {
            projects_by_user
                .entry(grant.approved_by)
                .or_default()
                .insert(grant.project_id);
        }
        let mut allowed = BTreeMap::new();
        // Same instance role ceiling as current_session_role. A scoped project
        // permission can narrow this ceiling, never widen it.
        if !Role::User
            .permissions()
            .contains(&Permission::ProjectsWrite)
            || !Role::User
                .permissions()
                .contains(&Permission::SandboxesWrite)
        {
            return Ok(allowed);
        }
        for (user_id, projects) in projects_by_user {
            let Some(checker) = checker else {
                allowed.extend(projects.into_iter().map(|project| {
                    (
                        (user_id, project),
                        Role::User
                            .permissions()
                            .contains(&Permission::GitRepositoriesRead),
                    )
                }));
                continue;
            };
            let projects: Vec<_> = projects.into_iter().collect();
            let permissions = checker
                .effective_project_permissions_batch(user_id, &projects)
                .await
                .map_err(|source| PlanError::Permission {
                    boot_id,
                    user_id,
                    source,
                })?;
            if projects.iter().any(|id| !permissions.contains_key(id)) {
                return Err(invalid(
                    boot_id,
                    "recheck project permissions",
                    "checker returned an incomplete permission batch",
                ));
            }
            let fallback: Vec<_> = projects
                .iter()
                .copied()
                .filter(|id| matches!(permissions.get(id), Some(None)))
                .collect();
            let access = if fallback.is_empty() {
                BTreeMap::new()
            } else {
                checker
                    .user_can_access_projects(user_id, &fallback)
                    .await
                    .map_err(|source| PlanError::Permission {
                        boot_id,
                        user_id,
                        source,
                    })?
            };
            if fallback.iter().any(|id| !access.contains_key(id)) {
                return Err(invalid(
                    boot_id,
                    "recheck project access",
                    "checker returned an incomplete access batch",
                ));
            }
            for project in projects {
                let base_permissions = [Permission::ProjectsWrite, Permission::SandboxesWrite];
                let permit = match permissions.get(&project) {
                    Some(Some(perms)) => base_permissions
                        .iter()
                        .all(|p| perms.contains(&p.to_string())),
                    Some(None) => access.get(&project) == Some(&true),
                    None => false,
                };
                if permit {
                    let github = Role::User
                        .permissions()
                        .contains(&Permission::GitRepositoriesRead)
                        && match permissions.get(&project) {
                            Some(Some(perms)) => {
                                perms.contains(&Permission::GitRepositoriesRead.to_string())
                            }
                            Some(None) => true,
                            None => false,
                        };
                    allowed.insert((user_id, project), github);
                }
            }
        }
        Ok(allowed)
    }
}
