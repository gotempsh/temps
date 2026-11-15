// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resource confinement for API keys, in addition to their permission ceiling.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyScope {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Debug)]
pub struct InvalidApiKeyScope {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

impl std::fmt::Display for InvalidApiKeyScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Invalid API key resource scope: project {:?}, environment {:?}",
            self.project_id, self.environment_id
        )
    }
}
impl std::error::Error for InvalidApiKeyScope {}

impl ApiKeyScope {
    pub fn from_ids(
        project_id: Option<i32>,
        environment_id: Option<i32>,
    ) -> Result<Option<Self>, InvalidApiKeyScope> {
        match (project_id, environment_id) {
            (None, None) => Ok(None),
            (Some(project_id), environment_id)
                if project_id > 0 && environment_id.is_none_or(|id| id > 0) =>
            {
                Ok(Some(Self {
                    project_id,
                    environment_id,
                }))
            }
            _ => Err(InvalidApiKeyScope {
                project_id,
                environment_id,
            }),
        }
    }

    /// Instance-wide and by-ID APIs without a project path are refused. This
    /// also prevents a scoped key from minting, rotating or widening API keys.
    pub fn allows_path(&self, path: &str) -> bool {
        let path = path
            .strip_prefix("/api/")
            .unwrap_or_else(|| path.strip_prefix('/').unwrap_or(path));
        let mut segments = path.split('/');
        if segments.next() != Some("projects")
            || segments.next().and_then(|id| id.parse::<i32>().ok()) != Some(self.project_id)
        {
            return false;
        }
        let resource = segments.next();
        let Some(environment_id) = self.environment_id else {
            // Deployment credentials do not enter AI/tool/plugin APIs, whose
            // delegated work can outlive the request and has its own authority.
            return matches!(
                resource,
                None | Some(
                    "deployments"
                        | "last-deployment"
                        | "environments"
                        | "trigger-pipeline"
                        | "deployment-config"
                        | "automatic-deploy"
                        | "containers"
                )
            );
        };
        match resource {
            // This endpoint carries its environment in a typed body. The
            // handler checks it before any lookup, write or audit event.
            Some("trigger-pipeline") => segments.next().is_none(),
            Some("environments") => {
                segments.next().and_then(|id| id.parse::<i32>().ok()) == Some(environment_id)
                    && matches!(
                        segments.next(),
                        None | Some(
                            "variables"
                                | "domains"
                                | "ip-access"
                                | "deployments"
                                | "logs"
                                | "resource-limits"
                                | "scaling"
                        )
                    )
            }
            _ => false,
        }
    }

    pub fn allows_environment(&self, project_id: i32, environment_id: i32) -> bool {
        self.project_id == project_id && self.environment_id.is_none_or(|id| id == environment_id)
    }
}

/// Shared by both middleware adapters and extractors used by in-process API callers.
pub fn enforce_api_key_path(
    auth: &crate::AuthContext,
    path: &str,
) -> Result<(), temps_core::problemdetails::Problem> {
    if let Some(scope) = auth.api_key_scope() {
        if !scope.allows_path(path) {
            return Err(temps_core::error_builder::ErrorBuilder::new(
                axum::http::StatusCode::FORBIDDEN,
            )
            .type_("https://temps.sh/probs/api-key-scope-denied")
            .title("API Key Scope Denied")
            .detail(format!(
                "This API key is confined to project {}{}; this endpoint is outside that scope",
                scope.project_id,
                scope
                    .environment_id
                    .map(|id| format!(" environment {id}"))
                    .unwrap_or_default()
            ))
            .build());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_scope_fails_closed() {
        assert_eq!(ApiKeyScope::from_ids(None, None).unwrap(), None);
        for (project, env) in [(None, Some(1)), (Some(0), None), (Some(1), Some(-1))] {
            assert!(ApiKeyScope::from_ids(project, env).is_err());
        }
    }

    #[test]
    fn project_keys_refuse_other_projects_and_instance_routes() {
        let scope = ApiKeyScope {
            project_id: 7,
            environment_id: None,
        };
        for path in [
            "/projects/7",
            "/api/projects/7/deployments",
            "/projects/7/trigger-pipeline",
        ] {
            assert!(scope.allows_path(path), "{path}");
        }
        for path in [
            "/projects/8",
            "/projects/70/deployments",
            "/deployments/7",
            "/api-keys",
            "/api/settings",
            "/users",
            "/projects",
            "/projects/invalid",
            "/projects/7/ai/chat",
            "/projects/7/new-unreviewed-feature",
        ] {
            assert!(!scope.allows_path(path), "{path}");
        }
    }

    #[test]
    fn environment_keys_cannot_read_or_mutate_sibling_environments() {
        let scope = ApiKeyScope {
            project_id: 7,
            environment_id: Some(9),
        };
        assert!(scope.allows_path("/api/projects/7/environments/9/variables"));
        assert!(scope.allows_path("/projects/7/trigger-pipeline"));
        for path in [
            "/projects/7",
            "/projects/7/deployments",
            "/projects/7/last-deployment",
            "/projects/7/environments",
            "/projects/7/environments/8",
            "/projects/8/environments/9",
            "/projects/7/trigger-pipeline/extra",
        ] {
            assert!(!scope.allows_path(path), "{path}");
        }
        assert!(scope.allows_environment(7, 9));
        assert!(!scope.allows_environment(7, 8));
        assert!(!scope.allows_environment(8, 9));
    }
}
