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
        // CLI login reads only the authenticated owner's existing identity.
        // This GET-only route cannot mint credentials or mutate the account.
        if path == "user/me" {
            return true;
        }
        let mut segments = path.split('/');
        if segments.next() != Some("projects")
            || segments.next().and_then(|id| id.parse::<i32>().ok()) != Some(self.project_id)
        {
            return false;
        }
        match segments.next() {
            Some("environments") => self.allows_environment_path(segments),
            // Static uploads stage caller-supplied content for this project;
            // deployment still requires the bound environment's deploy route.
            // No artifact listing, retrieval or other upload namespace is open.
            Some("upload") => segments.next() == Some("static") && segments.next().is_none(),
            Some("trigger-pipeline") => segments.next().is_none(),
            resource if self.environment_id.is_none() => matches!(
                resource,
                None | Some(
                    "deployments"
                        | "last-deployment"
                        | "deployment-config"
                        | "automatic-deploy"
                        | "containers"
                )
            ),
            _ => false,
        }
    }

    fn allows_environment_path<'a>(&self, mut segments: impl Iterator<Item = &'a str>) -> bool {
        let Some(environment) = segments.next() else {
            return self.environment_id.is_none();
        };
        let Some(environment_id) = environment.parse::<i32>().ok().filter(|id| *id > 0) else {
            return false;
        };
        if !self.allows_environment(self.project_id, environment_id) {
            return false;
        }
        match segments.next() {
            None => true,
            Some("deploy") => match segments.next() {
                Some("source" | "image" | "static") => segments.next().is_none(),
                Some("image-upload") => match segments.next() {
                    None => true,
                    Some(request_id) => !request_id.is_empty() && segments.next().is_none(),
                },
                _ => false,
            },
            // Both project and environment keys use the same reviewed route
            // namespaces. In particular, workflows/credential is never admitted.
            Some(
                "variables" | "domains" | "ip-access" | "deployments" | "logs" | "resource-limits"
                | "scaling",
            ) => true,
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
    fn project_scope_does_not_admit_environment_workflow_credentials() {
        for environment_id in [None, Some(9)] {
            let scope = ApiKeyScope {
                project_id: 7,
                environment_id,
            };
            for suffix in [
                "workflows/credential",
                "workflows/settings",
                "workflows",
                "new-credential-api",
            ] {
                assert!(
                    !scope.allows_path(&format!("/api/projects/7/environments/9/{suffix}")),
                    "{suffix}"
                );
            }
        }
    }

    #[test]
    fn scoped_keys_can_use_only_bound_reviewed_upload_deployment_routes() {
        for environment_id in [None, Some(9)] {
            let scope = ApiKeyScope {
                project_id: 7,
                environment_id,
            };
            for suffix in [
                "source",
                "image",
                "static",
                "image-upload",
                "image-upload/attempt",
            ] {
                assert!(
                    scope.allows_path(&format!("/api/projects/7/environments/9/deploy/{suffix}")),
                    "{suffix}"
                );
                assert!(
                    !scope.allows_path(&format!("/api/projects/8/environments/9/deploy/{suffix}"))
                );
                if environment_id.is_some() {
                    assert!(!scope
                        .allows_path(&format!("/api/projects/7/environments/8/deploy/{suffix}")));
                }
            }
            for suffix in [
                "credential",
                "new-route",
                "image/extra",
                "image-upload/attempt/extra",
            ] {
                assert!(
                    !scope.allows_path(&format!("/api/projects/7/environments/9/deploy/{suffix}"))
                );
            }
        }
    }

    #[test]
    fn static_bundle_staging_is_project_bound_and_does_not_open_artifact_access() {
        for environment_id in [None, Some(9)] {
            let scope = ApiKeyScope {
                project_id: 7,
                environment_id,
            };
            assert!(scope.allows_path("/api/projects/7/upload/static"));
            for path in [
                "/api/projects/8/upload/static",
                "/api/projects/7/upload/source",
                "/api/projects/7/upload/static/extra",
                "/api/projects/7/static-bundles",
                "/api/projects/7/static-bundles/1",
            ] {
                assert!(!scope.allows_path(path), "{path}");
            }
            assert!(scope.allows_path("/api/projects/7/environments/9/deploy/static"));
            if environment_id.is_some() {
                assert!(!scope.allows_path("/api/projects/7/environments/8/deploy/static"));
            }
        }
    }

    #[test]
    fn scoped_cli_login_can_read_own_identity_without_account_management() {
        for environment_id in [None, Some(9)] {
            let scope = ApiKeyScope {
                project_id: 7,
                environment_id,
            };
            assert!(scope.allows_path("/api/user/me"));
            assert!(scope.allows_path("/user/me"));
            for path in [
                "/users",
                "/user/8",
                "/user/me/password",
                "/api-keys",
                "/api/settings",
            ] {
                assert!(!scope.allows_path(path));
            }
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
