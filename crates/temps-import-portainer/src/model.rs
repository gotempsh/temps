//! Typed models for the Portainer REST API.
//!
//! Field sets captured from a live Portainer CE 2.39 instance (July 2026).
//! Portainer uses PascalCase JSON. The Docker proxy endpoints return stock
//! Docker Engine payloads, so only the fields the importer needs are modelled.

use serde::Deserialize;
use std::collections::HashMap;

/// Compose label identifying which stack/project a container belongs to
pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
/// Compose label identifying the service name inside a stack
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// `POST /api/auth` response
#[derive(Debug, Clone, Deserialize)]
pub struct PortainerAuth {
    pub jwt: String,
}

/// `GET /api/endpoints` element (a managed Docker environment)
#[derive(Debug, Clone, Deserialize)]
pub struct PortainerEndpoint {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "Name")]
    pub name: String,
    /// 1 = local Docker, 2 = agent, 3 = Azure, 4 = edge agent, 5 = K8s local…
    #[serde(default, rename = "Type")]
    pub endpoint_type: i64,
    /// 1 = up, 2 = down
    #[serde(default, rename = "Status")]
    pub status: i64,
    #[serde(default, rename = "URL")]
    pub url: Option<String>,
}

impl PortainerEndpoint {
    pub fn is_up(&self) -> bool {
        self.status == 1
    }
}

/// `GET /api/stacks` element
#[derive(Debug, Clone, Deserialize)]
pub struct PortainerStack {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "Name")]
    pub name: String,
    /// 1 = swarm, 2 = compose
    #[serde(default, rename = "Type")]
    pub stack_type: i64,
    #[serde(default, rename = "EndpointId")]
    pub endpoint_id: i64,
    /// 1 = active, 2 = inactive
    #[serde(default, rename = "Status")]
    pub status: i64,
    #[serde(default, rename = "EntryPoint")]
    pub entry_point: Option<String>,
}

/// `GET /api/stacks/{id}/file`
#[derive(Debug, Clone, Deserialize)]
pub struct PortainerStackFile {
    #[serde(rename = "StackFileContent")]
    pub content: String,
}

/// Docker container summary via Portainer's proxy
#[derive(Debug, Clone, Deserialize)]
pub struct DockerContainer {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(default, rename = "Names")]
    pub names: Vec<String>,
    #[serde(default, rename = "Image")]
    pub image: Option<String>,
    /// "running", "exited", "created", "paused"…
    #[serde(default, rename = "State")]
    pub state: Option<String>,
    #[serde(default, rename = "Labels")]
    pub labels: HashMap<String, String>,
}

impl DockerContainer {
    /// Container name without Docker's leading slash
    pub fn name(&self) -> String {
        self.names
            .first()
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| self.id.chars().take(12).collect())
    }

    /// The compose stack this container belongs to, if any
    pub fn stack(&self) -> Option<&str> {
        self.labels.get(COMPOSE_PROJECT_LABEL).map(|s| s.as_str())
    }

    /// The compose service name (falls back to the container name)
    pub fn service(&self) -> String {
        self.labels
            .get(COMPOSE_SERVICE_LABEL)
            .cloned()
            .unwrap_or_else(|| self.name())
    }

    /// Portainer's own containers must never be imported
    pub fn is_portainer_infra(&self) -> bool {
        self.labels.contains_key("io.portainer.server")
            || self
                .image
                .as_deref()
                .is_some_and(|i| i.contains("portainer/"))
    }
}

/// Docker container inspect via Portainer's proxy (only what we need)
#[derive(Debug, Clone, Deserialize)]
pub struct DockerContainerDetail {
    #[serde(default, rename = "Name")]
    pub name: Option<String>,
    #[serde(rename = "Config")]
    pub config: DockerContainerConfig,
    #[serde(default, rename = "NetworkSettings")]
    pub network_settings: Option<DockerNetworkSettings>,
    #[serde(default, rename = "Mounts")]
    pub mounts: Vec<DockerMount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerContainerConfig {
    #[serde(default, rename = "Image")]
    pub image: Option<String>,
    /// Raw `KEY=value` strings
    #[serde(default, rename = "Env")]
    pub env: Vec<String>,
    #[serde(default, rename = "Labels")]
    pub labels: HashMap<String, String>,
}

impl DockerContainerConfig {
    /// Parse Docker's `KEY=value` env list, dropping image-provided noise
    /// that would be wrong to carry across (PATH and friends).
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        const SKIP: &[&str] = &["PATH", "HOME", "HOSTNAME", "TERM", "LANG", "LC_ALL"];
        self.env
            .iter()
            .filter_map(|entry| entry.split_once('='))
            .filter(|(key, _)| !SKIP.contains(key))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    pub fn env(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .filter_map(|e| e.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerNetworkSettings {
    /// `"5432/tcp" -> [{HostIp, HostPort}]`; null when unpublished
    #[serde(default, rename = "Ports")]
    pub ports: HashMap<String, Option<Vec<DockerPortBinding>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerPortBinding {
    #[serde(default, rename = "HostIp")]
    pub host_ip: Option<String>,
    #[serde(default, rename = "HostPort")]
    pub host_port: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerMount {
    #[serde(default, rename = "Type")]
    pub mount_type: Option<String>,
    #[serde(default, rename = "Name")]
    pub name: Option<String>,
    #[serde(default, rename = "Destination")]
    pub destination: Option<String>,
}

impl DockerContainerDetail {
    /// Container port -> published host port (only published ones)
    pub fn published_ports(&self) -> Vec<(u16, Option<u16>)> {
        let Some(settings) = &self.network_settings else {
            return vec![];
        };
        let mut ports: Vec<(u16, Option<u16>)> = settings
            .ports
            .iter()
            .filter_map(|(spec, bindings)| {
                let container_port = spec.split('/').next()?.parse::<u16>().ok()?;
                let host_port = bindings
                    .as_ref()
                    .and_then(|b| b.first())
                    .and_then(|b| b.host_port.as_ref())
                    .and_then(|p| p.parse::<u16>().ok());
                Some((container_port, host_port))
            })
            .collect();
        ports.sort_unstable();
        ports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real responses from a live Portainer CE 2.39 instance.
    const ENDPOINT_FIXTURE: &str =
        r#"{"Id":1,"Name":"local","Type":1,"Status":1,"URL":"unix:///var/run/docker.sock"}"#;
    const STACK_FIXTURE: &str = r#"{"Id":1,"Name":"lab","Type":2,"EndpointId":1,"Status":1,"EntryPoint":"docker-compose.yml"}"#;
    const CONTAINER_FIXTURE: &str = r#"{"Id":"abc123","Names":["/lab-lab-db-1"],"Image":"postgres:16","State":"running","Labels":{"com.docker.compose.project":"lab","com.docker.compose.service":"lab-db","com.docker.compose.container-number":"1"}}"#;
    const PORTAINER_SELF_FIXTURE: &str = r#"{"Id":"def456","Names":["/portainer"],"Image":"portainer/portainer-ce:latest","State":"running","Labels":{"io.portainer.server":"true"}}"#;
    const DETAIL_FIXTURE: &str = r#"{"Name":"/lab-lab-db-1","Config":{"Image":"postgres:16","Env":["POSTGRES_DB=labdb","POSTGRES_USER=lab","POSTGRES_PASSWORD=pw","PATH=/usr/local/sbin:/usr/local/bin","GOSU_VERSION=1.19","PGDATA=/var/lib/postgresql/data"],"Labels":{}},"NetworkSettings":{"Ports":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"},{"HostIp":"::","HostPort":"5432"}]}},"Mounts":[{"Type":"volume","Name":"lab_lab-db-data","Destination":"/var/lib/postgresql/data"}]}"#;

    #[test]
    fn parses_endpoint_and_status() {
        let endpoint: PortainerEndpoint = serde_json::from_str(ENDPOINT_FIXTURE).unwrap();
        assert_eq!(endpoint.id, 1);
        assert!(endpoint.is_up());
    }

    #[test]
    fn parses_stack() {
        let stack: PortainerStack = serde_json::from_str(STACK_FIXTURE).unwrap();
        assert_eq!(stack.name, "lab");
        assert_eq!(stack.stack_type, 2); // compose
        assert_eq!(stack.endpoint_id, 1);
    }

    #[test]
    fn container_exposes_name_stack_and_service() {
        let container: DockerContainer = serde_json::from_str(CONTAINER_FIXTURE).unwrap();
        assert_eq!(container.name(), "lab-lab-db-1");
        assert_eq!(container.stack(), Some("lab"));
        assert_eq!(container.service(), "lab-db");
        assert!(!container.is_portainer_infra());
    }

    #[test]
    fn portainer_own_container_is_flagged() {
        let container: DockerContainer = serde_json::from_str(PORTAINER_SELF_FIXTURE).unwrap();
        assert!(container.is_portainer_infra());
    }

    #[test]
    fn detail_filters_image_noise_from_env() {
        let detail: DockerContainerDetail = serde_json::from_str(DETAIL_FIXTURE).unwrap();
        let env = detail.config.env_pairs();
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"POSTGRES_USER"));
        assert!(!keys.contains(&"PATH"), "PATH must not be carried over");
        assert_eq!(detail.config.env("POSTGRES_DB"), Some("labdb"));
    }

    #[test]
    fn detail_reports_published_ports() {
        let detail: DockerContainerDetail = serde_json::from_str(DETAIL_FIXTURE).unwrap();
        assert_eq!(detail.published_ports(), vec![(5432, Some(5432))]);
    }
}
