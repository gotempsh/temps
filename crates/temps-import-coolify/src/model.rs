// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed models for the Coolify REST API (`/api/v1`).
//!
//! Field sets were captured from a live Coolify v4 instance (July 2026) —
//! everything optional is `Option` with `serde(default)` so newer/older
//! Coolify versions that add or drop fields still deserialize.

use serde::Deserialize;

/// `GET /api/v1/servers` element
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyServer {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub ip: Option<String>,
}

/// `GET /api/v1/projects` element
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyProject {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GET /api/v1/projects/{uuid}` — includes the project's environments
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyProjectDetail {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub environments: Vec<CoolifyEnvironmentRef>,
}

/// Environment reference inside a project detail
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyEnvironmentRef {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub uuid: Option<String>,
}

/// `GET /api/v1/applications` element
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyApplication {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub environment_id: Option<i64>,
    #[serde(default)]
    pub git_repository: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub build_pack: Option<String>,
    /// Set when the repository needs an SSH deploy key — null means public
    #[serde(default)]
    pub private_key_id: Option<i64>,
    /// Full URL(s); Coolify separates multiple domains with commas
    #[serde(default)]
    pub fqdn: Option<String>,
    /// Exposed port(s) as a string, comma-separated (e.g. "3000" or "3000,9090")
    #[serde(default)]
    pub ports_exposes: Option<String>,
    #[serde(default)]
    pub docker_registry_image_name: Option<String>,
    #[serde(default)]
    pub docker_registry_image_tag: Option<String>,
    /// e.g. "running:unknown", "exited:unhealthy"
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub base_directory: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CoolifyApplication {
    /// Whether this application deploys a prebuilt registry image
    /// (`build_pack == "dockerimage"`). For those, `git_repository` holds a
    /// placeholder value and must be ignored.
    pub fn is_image_based(&self) -> bool {
        self.build_pack.as_deref() == Some("dockerimage")
    }

    /// Full image reference for image-based applications
    pub fn image_reference(&self) -> Option<String> {
        let name = self.docker_registry_image_name.as_deref()?;
        if name.is_empty() {
            return None;
        }
        let tag = self
            .docker_registry_image_tag
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or("latest");
        Some(format!("{}:{}", name, tag))
    }
}

/// `GET /api/v1/databases` element (standalone databases)
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyDatabase {
    pub uuid: String,
    pub name: String,
    /// e.g. "standalone-postgresql", "standalone-mysql", "standalone-redis"
    #[serde(default)]
    pub database_type: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub is_public: Option<bool>,
    #[serde(default)]
    pub public_port: Option<i32>,
    /// Connection URL on the instance's internal docker network
    #[serde(default)]
    pub internal_db_url: Option<String>,
    /// Connection URL reachable from outside (only meaningful when public)
    #[serde(default)]
    pub external_db_url: Option<String>,
    #[serde(default)]
    pub environment_id: Option<i64>,
    #[serde(default)]
    pub postgres_user: Option<String>,
    #[serde(default)]
    pub postgres_db: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CoolifyDatabase {
    /// Connection URL usable from outside the source box, if any.
    pub fn reachable_url(&self) -> Option<&str> {
        if self.is_public.unwrap_or(false) {
            self.external_db_url.as_deref()
        } else {
            None
        }
    }

    /// Database version parsed from the image tag (e.g. "postgres:16-alpine" -> "16")
    pub fn version_from_image(&self) -> Option<String> {
        let image = self.image.as_deref()?;
        let tag = image.split(':').nth(1)?;
        let version: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!version.is_empty()).then_some(version)
    }
}

/// `GET /api/v1/applications/{uuid}/envs` element
#[derive(Debug, Clone, Deserialize)]
pub struct CoolifyEnvVar {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
    /// Resolved value (Coolify templates like `{{team.VAR}}` expanded)
    #[serde(default)]
    pub real_value: Option<String>,
    #[serde(default)]
    pub is_preview: bool,
    /// Coolify's "shown once" flag marks operator-designated secrets
    #[serde(default)]
    pub is_shown_once: bool,
    /// Set by Coolify itself (e.g. build instructions), not by the user
    #[serde(default)]
    pub is_coolify: bool,
}

impl CoolifyEnvVar {
    /// The value to migrate: prefer the resolved value.
    pub fn effective_value(&self) -> &str {
        self.real_value
            .as_deref()
            .or(self.value.as_deref())
            .unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real responses captured from a live Coolify v4.x instance.
    const APP_FIXTURE: &str = r#"{"uuid":"aouhx3o71r5o38pvjw3gd24q","name":"lab-shop","environment_id":2,"git_repository":"heroku/node-js-getting-started","git_branch":"main","build_pack":"nixpacks","fqdn":"http://aouhx3o71r5o38pvjw3gd24q.167.233.223.170.sslip.io","ports_exposes":"5006","docker_registry_image_name":null,"docker_registry_image_tag":null,"status":"running:unknown","base_directory":"/","extra_field_from_future_version":true}"#;
    const IMAGE_APP_FIXTURE: &str = r#"{"uuid":"j81evy0of02xzeai6htmlyic","name":"lab-whoami","environment_id":2,"git_repository":"coollabsio/coolify","git_branch":"main","build_pack":"dockerimage","fqdn":"http://j81evy0of02xzeai6htmlyic.167.233.223.170.sslip.io","ports_exposes":"80","docker_registry_image_name":"traefik/whoami","docker_registry_image_tag":"latest","status":"running:unknown"}"#;
    const DB_FIXTURE: &str = r#"{"uuid":"e8xndfxu3it2lsuyug1hk6sd","name":"lab-db","database_type":"standalone-postgresql","image":"postgres:16-alpine","is_public":true,"public_port":5432,"internal_db_url":"postgres://postgres:pw@e8xndfxu3it2lsuyug1hk6sd:5432/postgres","external_db_url":"postgres://postgres:pw@167.233.223.170:5432/postgres","environment_id":2,"postgres_user":"postgres","postgres_db":"postgres"}"#;
    const ENV_FIXTURE: &str = r#"{"uuid":"y2ch741lj780w598jtu24dc1","comment":null,"is_buildtime":true,"is_coolify":false,"is_preview":false,"is_shown_once":false,"key":"NIXPACKS_NODE_VERSION","real_value":"22","value":"22","version":"4.1.2"}"#;
    const PROJECT_DETAIL_FIXTURE: &str = r#"{"uuid":"feyuvv97uak39yyfqbcx4n8n","name":"My first project","environments":[{"id":1,"name":"production","uuid":"sqjmx129h4y0kgpw3giem2ne"}]}"#;

    #[test]
    fn parses_git_application_fixture() {
        let app: CoolifyApplication = serde_json::from_str(APP_FIXTURE).unwrap();
        assert_eq!(app.uuid, "aouhx3o71r5o38pvjw3gd24q");
        assert_eq!(app.environment_id, Some(2));
        assert_eq!(
            app.git_repository.as_deref(),
            Some("heroku/node-js-getting-started")
        );
        assert!(!app.is_image_based());
        assert_eq!(app.image_reference(), None);
    }

    #[test]
    fn parses_image_application_and_ignores_placeholder_git() {
        let app: CoolifyApplication = serde_json::from_str(IMAGE_APP_FIXTURE).unwrap();
        assert!(app.is_image_based());
        assert_eq!(
            app.image_reference().as_deref(),
            Some("traefik/whoami:latest")
        );
    }

    #[test]
    fn parses_database_fixture_with_reachability_and_version() {
        let db: CoolifyDatabase = serde_json::from_str(DB_FIXTURE).unwrap();
        assert_eq!(
            db.reachable_url(),
            Some("postgres://postgres:pw@167.233.223.170:5432/postgres")
        );
        assert_eq!(db.version_from_image().as_deref(), Some("16"));
        assert_eq!(db.database_type.as_deref(), Some("standalone-postgresql"));
    }

    #[test]
    fn private_database_has_no_reachable_url() {
        let mut db: CoolifyDatabase = serde_json::from_str(DB_FIXTURE).unwrap();
        db.is_public = Some(false);
        assert_eq!(db.reachable_url(), None);
    }

    #[test]
    fn parses_env_var_fixture() {
        let env: CoolifyEnvVar = serde_json::from_str(ENV_FIXTURE).unwrap();
        assert_eq!(env.key, "NIXPACKS_NODE_VERSION");
        assert_eq!(env.effective_value(), "22");
        assert!(!env.is_shown_once);
    }

    #[test]
    fn parses_project_detail_fixture() {
        let project: CoolifyProjectDetail = serde_json::from_str(PROJECT_DETAIL_FIXTURE).unwrap();
        assert_eq!(project.environments.len(), 1);
        assert_eq!(project.environments[0].id, 1);
        assert_eq!(project.environments[0].name, "production");
    }
}
