// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed models for the CapRover REST API (`/api/v2`).
//!
//! Field sets captured from a live CapRover instance (July 2026). CapRover
//! wraps every response in `{status, description, data}` — status 100 = OK.

use serde::Deserialize;

/// Response envelope used by every CapRover endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverEnvelope<T> {
    pub status: i64,
    #[serde(default)]
    pub description: Option<String>,
    pub data: Option<T>,
}

/// CapRover's "everything is fine" status code
pub const CAPROVER_STATUS_OK: i64 = 100;
/// Wrong password on login
pub const CAPROVER_STATUS_AUTH_FAILED: i64 = 1105;

/// `POST /api/v2/login` payload data
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverLogin {
    pub token: String,
}

/// `GET /api/v2/user/system/info` payload data
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverSystemInfo {
    #[serde(default, rename = "rootDomain")]
    pub root_domain: Option<String>,
    #[serde(default, rename = "forceSsl")]
    pub force_ssl: bool,
}

/// `GET /api/v2/user/apps/appDefinitions` payload data
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverAppList {
    #[serde(default, rename = "appDefinitions")]
    pub app_definitions: Vec<CaproverApp>,
}

/// One CapRover app definition
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverApp {
    #[serde(rename = "appName")]
    pub app_name: String,
    #[serde(default, rename = "hasPersistentData")]
    pub has_persistent_data: bool,
    #[serde(default, rename = "instanceCount")]
    pub instance_count: i64,
    #[serde(default, rename = "containerHttpPort")]
    pub container_http_port: Option<u16>,
    #[serde(default, rename = "notExposeAsWebApp")]
    pub not_expose_as_web_app: bool,
    #[serde(default, rename = "forceSsl")]
    pub force_ssl: bool,
    #[serde(default, rename = "envVars")]
    pub env_vars: Vec<CaproverEnvVar>,
    #[serde(default)]
    pub ports: Vec<CaproverPort>,
    #[serde(default)]
    pub volumes: Vec<CaproverVolume>,
    #[serde(default, rename = "customDomain")]
    pub custom_domain: Vec<CaproverCustomDomain>,
    #[serde(default, rename = "deployedVersion")]
    pub deployed_version: Option<i64>,
    #[serde(default)]
    pub versions: Vec<CaproverVersion>,
    #[serde(default, rename = "appPushWebhook")]
    pub app_push_webhook: Option<CaproverPushWebhook>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CaproverApp {
    /// The image currently deployed (resolved through the versions list)
    pub fn deployed_image(&self) -> Option<&str> {
        let version = self.deployed_version?;
        self.versions
            .iter()
            .find(|v| v.version == Some(version))
            .and_then(|v| v.deployed_image_name.as_deref())
            .filter(|image| !image.is_empty())
    }

    /// Env var lookup helper
    pub fn env(&self, key: &str) -> Option<&str> {
        self.env_vars
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.value.as_str())
    }

    /// Git repo configuration, if the app deploys from a repository
    pub fn repo_info(&self) -> Option<&CaproverRepoInfo> {
        self.app_push_webhook
            .as_ref()
            .and_then(|w| w.repo_info.as_ref())
            .filter(|r| !r.repo.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverEnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverPort {
    #[serde(default, rename = "containerPort")]
    pub container_port: Option<u16>,
    #[serde(default, rename = "hostPort")]
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverVolume {
    #[serde(default, rename = "volumeName")]
    pub volume_name: Option<String>,
    #[serde(default, rename = "containerPath")]
    pub container_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverCustomDomain {
    #[serde(default, rename = "publicDomain")]
    pub public_domain: Option<String>,
    #[serde(default, rename = "hasSsl")]
    pub has_ssl: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverVersion {
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default, rename = "deployedImageName")]
    pub deployed_image_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaproverPushWebhook {
    #[serde(default, rename = "repoInfo")]
    pub repo_info: Option<CaproverRepoInfo>,
}

/// Git repository configuration. `password` is a secret — never copy it
/// into snapshots or plans.
#[derive(Debug, Clone, Deserialize)]
pub struct CaproverRepoInfo {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real response captured from a live CapRover instance.
    const APP_FIXTURE: &str = r#"{"appName":"lab-db","hasPersistentData":true,"instanceCount":1,"containerHttpPort":80,"notExposeAsWebApp":true,"envVars":[{"key":"POSTGRES_USER","value":"lab"},{"key":"POSTGRES_PASSWORD","value":"pw"},{"key":"POSTGRES_DB","value":"labdb"}],"ports":[{"hostPort":5432,"containerPort":5432}],"volumes":[{"containerPath":"/var/lib/postgresql/data","volumeName":"lab-db-data"}],"customDomain":[],"deployedVersion":1,"versions":[{"version":0,"deployedImageName":""},{"version":1,"deployedImageName":"postgres:16-alpine"}],"appPushWebhook":null}"#;
    const GIT_APP_FIXTURE: &str = r#"{"appName":"lab-shop","hasPersistentData":false,"instanceCount":1,"containerHttpPort":3000,"notExposeAsWebApp":false,"envVars":[{"key":"PLAN","value":"pro"}],"ports":[],"volumes":[],"customDomain":[{"publicDomain":"shop.example.com","hasSsl":true}],"deployedVersion":1,"versions":[{"version":1,"deployedImageName":"node:22-alpine"}],"appPushWebhook":{"repoInfo":{"repo":"github.com/heroku/node-js-getting-started","user":"public","password":"secret","sshKey":"","branch":"main"}}}"#;
    const ENVELOPE_FIXTURE: &str =
        r#"{"status":100,"description":"OK","data":{"appDefinitions":[]}}"#;

    #[test]
    fn parses_envelope() {
        let env: CaproverEnvelope<CaproverAppList> =
            serde_json::from_str(ENVELOPE_FIXTURE).unwrap();
        assert_eq!(env.status, CAPROVER_STATUS_OK);
        assert!(env.data.unwrap().app_definitions.is_empty());
    }

    #[test]
    fn parses_database_app_with_deployed_image_and_ports() {
        let app: CaproverApp = serde_json::from_str(APP_FIXTURE).unwrap();
        assert!(app.has_persistent_data);
        assert_eq!(app.deployed_image(), Some("postgres:16-alpine"));
        assert_eq!(app.env("POSTGRES_USER"), Some("lab"));
        assert_eq!(app.ports[0].host_port, Some(5432));
        assert!(app.repo_info().is_none());
    }

    #[test]
    fn parses_git_app_with_repo_info_and_custom_domain() {
        let app: CaproverApp = serde_json::from_str(GIT_APP_FIXTURE).unwrap();
        let repo = app.repo_info().unwrap();
        assert_eq!(repo.repo, "github.com/heroku/node-js-getting-started");
        assert_eq!(repo.branch, "main");
        assert_eq!(
            app.custom_domain[0].public_domain.as_deref(),
            Some("shop.example.com")
        );
    }
}
