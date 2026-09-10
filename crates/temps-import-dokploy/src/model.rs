// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed models for the Dokploy REST API.
//!
//! Field sets captured from a live Dokploy v0.29 instance (July 2026).
//! Everything optional is `Option` with `serde(default)` so other versions
//! still deserialize.

use serde::Deserialize;

/// `GET /api/project.all` element
#[derive(Debug, Clone, Deserialize)]
pub struct DokployProject {
    #[serde(rename = "projectId")]
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub environments: Vec<DokployEnvironment>,
}

/// Environment inside a project (embeds children as brief stubs)
#[derive(Debug, Clone, Deserialize)]
pub struct DokployEnvironment {
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    pub name: String,
    #[serde(default, rename = "isDefault")]
    pub is_default: bool,
    #[serde(default)]
    pub applications: Vec<DokployApplicationStub>,
    #[serde(default)]
    pub postgres: Vec<DokployDbStub>,
    #[serde(default)]
    pub mysql: Vec<DokployDbStub>,
    #[serde(default)]
    pub mariadb: Vec<DokployDbStub>,
    #[serde(default)]
    pub mongo: Vec<DokployDbStub>,
    #[serde(default)]
    pub redis: Vec<DokployDbStub>,
    #[serde(default)]
    pub compose: Vec<serde_json::Value>,
}

/// Application stub inside `project.all` (id + name + status only)
#[derive(Debug, Clone, Deserialize)]
pub struct DokployApplicationStub {
    #[serde(rename = "applicationId")]
    pub application_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "applicationStatus")]
    pub application_status: Option<String>,
}

/// Database stub inside `project.all`. Postgres stubs carry only the id;
/// name/details come from the per-type `.one` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct DokployDbStub {
    #[serde(
        default,
        alias = "postgresId",
        alias = "mysqlId",
        alias = "mariadbId",
        alias = "mongoId",
        alias = "redisId"
    )]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `GET /api/application.one?applicationId=…`
#[derive(Debug, Clone, Deserialize)]
pub struct DokployApplication {
    #[serde(rename = "applicationId")]
    pub application_id: String,
    pub name: String,
    /// Container-side name (docker resource prefix)
    #[serde(default, rename = "appName")]
    pub app_name: Option<String>,
    /// "git" (custom repo), "github"/"gitlab"/…, or "docker"
    #[serde(default, rename = "sourceType")]
    pub source_type: Option<String>,
    #[serde(default, rename = "customGitUrl")]
    pub custom_git_url: Option<String>,
    #[serde(default, rename = "customGitBranch")]
    pub custom_git_branch: Option<String>,
    #[serde(default, rename = "customGitBuildPath")]
    pub custom_git_build_path: Option<String>,
    /// GitHub-app sourced repos
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default, rename = "dockerImage")]
    pub docker_image: Option<String>,
    /// "nixpacks", "dockerfile", "heroku_buildpacks", …
    #[serde(default, rename = "buildType")]
    pub build_type: Option<String>,
    /// Newline-separated K=V lines
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default, rename = "applicationStatus")]
    pub application_status: Option<String>,
    #[serde(default)]
    pub domains: Vec<DokployDomain>,
    #[serde(default)]
    pub description: Option<String>,
}

impl DokployApplication {
    pub fn is_image_based(&self) -> bool {
        self.source_type.as_deref() == Some("docker")
    }

    /// Parse Dokploy's newline-separated env block into pairs
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        self.env
            .as_deref()
            .unwrap_or("")
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (key, value) = line.split_once('=')?;
                Some((key.trim().to_string(), value.trim().to_string()))
            })
            .collect()
    }
}

/// Domain attached to an application
#[derive(Debug, Clone, Deserialize)]
pub struct DokployDomain {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub https: Option<bool>,
    #[serde(default, rename = "domainType")]
    pub domain_type: Option<String>,
}

/// `GET /api/{postgres,mysql,mariadb,mongo,redis}.one` — the common fields
#[derive(Debug, Clone, Deserialize)]
pub struct DokployDatabase {
    #[serde(
        default,
        alias = "postgresId",
        alias = "mysqlId",
        alias = "mariadbId",
        alias = "mongoId",
        alias = "redisId"
    )]
    pub id: Option<String>,
    pub name: String,
    #[serde(default, rename = "appName")]
    pub app_name: Option<String>,
    #[serde(default, rename = "databaseName")]
    pub database_name: Option<String>,
    #[serde(default, rename = "databaseUser")]
    pub database_user: Option<String>,
    #[serde(default, rename = "databasePassword")]
    pub database_password: Option<String>,
    #[serde(default, rename = "dockerImage")]
    pub docker_image: Option<String>,
    /// Host port when the database is exposed externally; null otherwise
    #[serde(default, rename = "externalPort")]
    pub external_port: Option<u16>,
    #[serde(default, rename = "applicationStatus")]
    pub application_status: Option<String>,
}

impl DokployDatabase {
    /// Database version parsed from the image tag (e.g. "postgres:16-alpine" -> "16")
    pub fn version_from_image(&self) -> Option<String> {
        let image = self.docker_image.as_deref()?;
        let tag = image.split(':').nth(1)?;
        let version: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!version.is_empty()).then_some(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real responses captured from a live Dokploy v0.29.13 instance.
    const PROJECT_ALL_FIXTURE: &str = r#"[{"projectId":"PpqBfKQLtptI1rc93zxLl","name":"lab","description":"migration-lab seeded workload","environments":[{"environmentId":"4I5yGvBx23BMZ8NtkfYR-","name":"production","isDefault":true,"applications":[{"applicationId":"Sl8mKkYtbgUmo-1aOV9Rk","name":"lab-whoami","applicationStatus":"idle"},{"applicationId":"i5WD-1zZfSw4Zoo6RbUNW","name":"lab-shop","applicationStatus":"running"}],"postgres":[{"postgresId":"C2YCHJysf-IiNTGIbY_3e"}],"mysql":[],"mariadb":[],"mongo":[],"redis":[],"compose":[]}]}]"#;
    const APP_GIT_FIXTURE: &str = r#"{"applicationId":"i5WD-1zZfSw4Zoo6RbUNW","name":"lab-shop","appName":"app-x","sourceType":"git","customGitUrl":"https://github.com/heroku/node-js-getting-started.git","customGitBranch":"main","customGitBuildPath":"/","buildType":"nixpacks","env":"LAB_SECRET=s3cret\nPLAN=pro\nFEATURE_CHECKOUT=true","applicationStatus":"running","domains":[]}"#;
    const APP_DOCKER_FIXTURE: &str = r#"{"applicationId":"Sl8mKkYtbgUmo-1aOV9Rk","name":"lab-whoami","appName":"app-y","sourceType":"docker","customGitUrl":null,"customGitBranch":null,"dockerImage":"traefik/whoami:latest","buildType":"nixpacks","env":null,"applicationStatus":"idle","domains":[{"host":"whoami.10.0.0.1.traefik.me","https":false}]}"#;
    const PG_FIXTURE: &str = r#"{"postgresId":"C2YCHJysf-IiNTGIbY_3e","name":"lab-db","appName":"postgres-x","databaseName":"labdb","databaseUser":"lab","databasePassword":"pw","dockerImage":"postgres:16-alpine","externalPort":null,"applicationStatus":"done"}"#;

    #[test]
    fn parses_project_all_tree() {
        let projects: Vec<DokployProject> = serde_json::from_str(PROJECT_ALL_FIXTURE).unwrap();
        assert_eq!(projects.len(), 1);
        let env = &projects[0].environments[0];
        assert_eq!(env.name, "production");
        assert!(env.is_default);
        assert_eq!(env.applications.len(), 2);
        assert_eq!(env.postgres.len(), 1);
        assert_eq!(env.postgres[0].id.as_deref(), Some("C2YCHJysf-IiNTGIbY_3e"));
    }

    #[test]
    fn parses_git_application_with_env_lines() {
        let app: DokployApplication = serde_json::from_str(APP_GIT_FIXTURE).unwrap();
        assert!(!app.is_image_based());
        assert_eq!(
            app.custom_git_url.as_deref(),
            Some("https://github.com/heroku/node-js-getting-started.git")
        );
        let env = app.env_pairs();
        assert_eq!(env.len(), 3);
        assert_eq!(env[0], ("LAB_SECRET".to_string(), "s3cret".to_string()));
    }

    #[test]
    fn parses_docker_application_with_domain() {
        let app: DokployApplication = serde_json::from_str(APP_DOCKER_FIXTURE).unwrap();
        assert!(app.is_image_based());
        assert_eq!(app.docker_image.as_deref(), Some("traefik/whoami:latest"));
        assert_eq!(
            app.domains[0].host.as_deref(),
            Some("whoami.10.0.0.1.traefik.me")
        );
        assert!(app.env_pairs().is_empty());
    }

    #[test]
    fn parses_postgres_with_version_and_no_external_port() {
        let db: DokployDatabase = serde_json::from_str(PG_FIXTURE).unwrap();
        assert_eq!(db.version_from_image().as_deref(), Some("16"));
        assert_eq!(db.external_port, None);
        assert_eq!(db.database_user.as_deref(), Some("lab"));
    }
}
