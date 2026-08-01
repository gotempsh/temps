//! Typed model of Kamal's `config/deploy.yml`.
//!
//! Modelled against Kamal 2.12 (`kamal init` template + `kamal config`
//! resolution). Everything except `service` and `image` is optional, matching
//! Kamal's own defaults, so partial configs still parse.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Root of `config/deploy.yml`
#[derive(Debug, Clone, Deserialize)]
pub struct KamalConfig {
    /// Application name — used for container naming (required by Kamal)
    pub service: String,
    /// Image repository, e.g. `acme/lab-shop` (required by Kamal)
    pub image: String,
    /// Roles → hosts. Values are either a bare host list or a role object.
    #[serde(default)]
    pub servers: BTreeMap<String, KamalRole>,
    #[serde(default)]
    pub proxy: Option<KamalProxy>,
    #[serde(default)]
    pub registry: Option<KamalRegistry>,
    #[serde(default)]
    pub builder: Option<KamalBuilder>,
    #[serde(default)]
    pub env: Option<KamalEnv>,
    /// Docker volume args, e.g. `app_storage:/app/storage`
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub accessories: BTreeMap<String, KamalAccessory>,
    #[serde(default)]
    pub asset_path: Option<String>,
    #[serde(default)]
    pub ssh: Option<KamalSsh>,
}

/// A role is either `- host` list shorthand or `{hosts: [...], cmd: ...}`
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum KamalRole {
    /// `web:\n  - 1.2.3.4`
    Hosts(Vec<String>),
    /// `job:\n  hosts: [...]\n  cmd: bin/jobs`
    Detailed(KamalRoleDetail),
}

#[derive(Debug, Clone, Deserialize)]
pub struct KamalRoleDetail {
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Command override for this role (workers, cron, …)
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub env: Option<KamalEnv>,
    /// A role with `proxy: false` is not web-facing
    #[serde(default)]
    pub proxy: Option<serde_yaml::Value>,
}

impl KamalRole {
    pub fn hosts(&self) -> &[String] {
        match self {
            KamalRole::Hosts(hosts) => hosts,
            KamalRole::Detailed(detail) => &detail.hosts,
        }
    }

    pub fn cmd(&self) -> Option<&str> {
        match self {
            KamalRole::Hosts(_) => None,
            KamalRole::Detailed(detail) => detail.cmd.as_deref(),
        }
    }

    pub fn env(&self) -> Option<&KamalEnv> {
        match self {
            KamalRole::Hosts(_) => None,
            KamalRole::Detailed(detail) => detail.env.as_ref(),
        }
    }

    /// Kamal's own rule: the `web` role is web-facing by default; other roles
    /// are only web-facing if they explicitly enable the proxy.
    pub fn is_web_facing(&self, role_name: &str) -> bool {
        match self {
            KamalRole::Hosts(_) => role_name == "web",
            KamalRole::Detailed(detail) => match &detail.proxy {
                Some(serde_yaml::Value::Bool(enabled)) => *enabled,
                Some(serde_yaml::Value::Null) | None => role_name == "web",
                Some(_) => true, // a proxy config object means proxied
            },
        }
    }
}

/// `proxy:` — kamal-proxy handles TLS and routing
#[derive(Debug, Clone, Deserialize)]
pub struct KamalProxy {
    #[serde(default)]
    pub ssl: Option<bool>,
    /// One host, or several comma-separated
    #[serde(default)]
    pub host: Option<String>,
    /// Container port kamal-proxy forwards to (Kamal defaults to 80)
    #[serde(default)]
    pub app_port: Option<u16>,
}

impl KamalProxy {
    /// Hosts declared on the proxy (Kamal accepts a comma-separated list)
    pub fn hosts(&self) -> Vec<String> {
        self.host
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KamalRegistry {
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub username: Option<serde_yaml::Value>,
    /// Usually `[SECRET_NAME]` — a reference, never a literal
    #[serde(default)]
    pub password: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KamalBuilder {
    #[serde(default)]
    pub arch: Option<serde_yaml::Value>,
    #[serde(default)]
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub args: BTreeMap<String, serde_yaml::Value>,
}

/// `env:` — literal values plus the *names* of secrets
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KamalEnv {
    #[serde(default)]
    pub clear: BTreeMap<String, serde_yaml::Value>,
    /// Secret names only; values live in `.kamal/secrets`, never in the config
    #[serde(default)]
    pub secret: Vec<String>,
}

impl KamalEnv {
    /// Literal `clear` values as strings (YAML scalars stringified the way
    /// Kamal would pass them to Docker).
    pub fn clear_pairs(&self) -> Vec<(String, String)> {
        self.clear
            .iter()
            .map(|(key, value)| (key.clone(), scalar_to_string(value)))
            .collect()
    }
}

/// `accessories:` — the databases/caches Kamal runs alongside the app
#[derive(Debug, Clone, Deserialize)]
pub struct KamalAccessory {
    pub image: String,
    /// Single host, or `hosts:`/`roles:` in richer configs
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Published port; Kamal accepts `3306` or `"127.0.0.1:3306:3306"`
    #[serde(default)]
    pub port: Option<serde_yaml::Value>,
    #[serde(default)]
    pub env: Option<KamalEnv>,
    /// `data:/var/lib/mysql` — presence means the accessory holds state
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub cmd: Option<String>,
}

impl KamalAccessory {
    /// The host this accessory runs on, if declared
    pub fn effective_host(&self) -> Option<&str> {
        self.host
            .as_deref()
            .or_else(|| self.hosts.first().map(|h| h.as_str()))
    }

    /// The published host port, parsed from either form
    pub fn host_port(&self) -> Option<u16> {
        match self.port.as_ref()? {
            serde_yaml::Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
            serde_yaml::Value::String(s) => {
                // "3306", "127.0.0.1:3306:3306" — the published port is the
                // second-to-last field when colons are present.
                let parts: Vec<&str> = s.split(':').collect();
                let candidate = if parts.len() >= 2 {
                    parts[parts.len() - 2]
                } else {
                    parts[0]
                };
                candidate.parse::<u16>().ok()
            }
            _ => None,
        }
    }

    /// Accessories with mounted directories keep data across deploys
    pub fn has_persistent_data(&self) -> bool {
        !self.directories.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KamalSsh {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<serde_yaml::Value>,
}

/// Stringify a YAML scalar the way Kamal passes it to Docker
pub fn scalar_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

impl KamalConfig {
    /// Parse `config/deploy.yml`.
    ///
    /// ERB (`<%= ... %>`) is rejected with an actionable message: Kamal
    /// evaluates it at deploy time against the local environment, so temps
    /// would silently import a wrong value.
    pub fn parse(yaml: &str) -> Result<Self, crate::error::KamalImportError> {
        use crate::error::KamalImportError;
        if yaml.trim().is_empty() {
            return Err(KamalImportError::MissingConfig);
        }
        // Only flag ERB on lines that survive as config (not comments).
        if let Some(line) = yaml
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.starts_with('#'))
            .find(|l| l.contains("<%="))
        {
            return Err(KamalImportError::ErbTemplating {
                field: line.chars().take(60).collect(),
            });
        }
        let config: KamalConfig = serde_yaml::from_str(yaml).map_err(|e| {
            let reason = e.to_string();
            // serde reports a missing required field as "missing field `x`"
            if reason.contains("missing field `service`") {
                return KamalImportError::MissingField { field: "service" };
            }
            if reason.contains("missing field `image`") {
                return KamalImportError::MissingField { field: "image" };
            }
            KamalImportError::InvalidYaml { reason }
        })?;
        Ok(config)
    }

    /// The role that represents the app itself: `web` when present,
    /// otherwise the first declared role.
    pub fn primary_role(&self) -> Option<(&String, &KamalRole)> {
        self.servers
            .get_key_value("web")
            .or_else(|| self.servers.iter().next())
    }

    /// Fully-qualified image reference including the registry server
    pub fn image_reference(&self) -> String {
        match self
            .registry
            .as_ref()
            .and_then(|r| r.server.as_deref())
            .filter(|s| !s.is_empty() && *s != "docker.io")
        {
            Some(server) => format!("{}/{}:latest", server, self.image),
            None => format!("{}:latest", self.image),
        }
    }

    /// Port the app listens on (Kamal's proxy defaults to 80)
    pub fn app_port(&self) -> u16 {
        self.proxy.as_ref().and_then(|p| p.app_port).unwrap_or(80)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic config, validated with `kamal config` (Kamal 2.12) before
    /// being frozen here.
    pub(crate) const FULL_CONFIG: &str = r#"
service: lab-shop
image: acme/lab-shop
servers:
  web:
    - 192.168.0.1
  job:
    hosts:
      - 192.168.0.2
    cmd: bin/jobs
proxy:
  ssl: true
  host: shop.example.com
  app_port: 3000
registry:
  server: ghcr.io
  username: acme-bot
  password:
    - KAMAL_REGISTRY_PASSWORD
builder:
  arch: amd64
  dockerfile: Dockerfile
env:
  clear:
    PLAN: pro
    FEATURE_CHECKOUT: "true"
    DB_HOST: 192.168.0.2
  secret:
    - RAILS_MASTER_KEY
    - LAB_SECRET
volumes:
  - "app_storage:/app/storage"
accessories:
  db:
    image: postgres:16
    host: 192.168.0.2
    port: 5432
    env:
      clear:
        POSTGRES_USER: lab
        POSTGRES_DB: labdb
      secret:
        - POSTGRES_PASSWORD
    directories:
      - data:/var/lib/postgresql/data
  redis:
    image: valkey/valkey:8
    host: 192.168.0.2
    port: 6379
    directories:
      - data:/data
"#;

    /// The stock `kamal init` template — everything optional is commented out.
    const MINIMAL_CONFIG: &str = r#"
service: my-app
image: my-user/my-app
servers:
  web:
    - 192.168.0.1
proxy:
  ssl: true
  host: app.example.com
registry:
  server: localhost:5555
builder:
  arch: amd64
"#;

    #[test]
    fn parses_full_config() {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        assert_eq!(config.service, "lab-shop");
        assert_eq!(config.accessories.len(), 2);
        assert_eq!(config.servers.len(), 2);
        assert_eq!(config.app_port(), 3000);
        assert_eq!(config.image_reference(), "ghcr.io/acme/lab-shop:latest");
    }

    #[test]
    fn parses_stock_init_template() {
        let config = KamalConfig::parse(MINIMAL_CONFIG).unwrap();
        assert_eq!(config.service, "my-app");
        assert!(config.accessories.is_empty());
        // no app_port declared -> Kamal's proxy default
        assert_eq!(config.app_port(), 80);
    }

    #[test]
    fn primary_role_prefers_web_and_reads_cmd() {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        let (name, role) = config.primary_role().unwrap();
        assert_eq!(name, "web");
        assert_eq!(role.hosts(), ["192.168.0.1"]);
        assert!(role.is_web_facing("web"));

        let job = &config.servers["job"];
        assert_eq!(job.cmd(), Some("bin/jobs"));
        assert!(!job.is_web_facing("job"), "non-web roles are not proxied");
    }

    #[test]
    fn env_separates_clear_values_from_secret_names() {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        let env = config.env.unwrap();
        let clear = env.clear_pairs();
        assert!(clear.contains(&("PLAN".to_string(), "pro".to_string())));
        // booleans-as-strings must survive verbatim
        assert!(clear.contains(&("FEATURE_CHECKOUT".to_string(), "true".to_string())));
        assert_eq!(env.secret, vec!["RAILS_MASTER_KEY", "LAB_SECRET"]);
    }

    #[test]
    fn accessory_exposes_host_port_and_persistence() {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        let db = &config.accessories["db"];
        assert_eq!(db.effective_host(), Some("192.168.0.2"));
        assert_eq!(db.host_port(), Some(5432));
        assert!(db.has_persistent_data());
        assert_eq!(
            db.env.as_ref().unwrap().secret,
            vec!["POSTGRES_PASSWORD".to_string()]
        );
    }

    #[test]
    fn accessory_port_accepts_binding_string_form() {
        let yaml = r#"
service: a
image: b/c
accessories:
  db:
    image: mysql:8.0
    port: "127.0.0.1:3306:3306"
"#;
        let config = KamalConfig::parse(yaml).unwrap();
        assert_eq!(config.accessories["db"].host_port(), Some(3306));
    }

    #[test]
    fn erb_is_rejected_with_guidance() {
        let yaml = r#"
service: a
image: b/c
env:
  clear:
    VERSION: <%= ENV["APP_VERSION"] %>
"#;
        let err = KamalConfig::parse(yaml).unwrap_err();
        assert!(matches!(
            err,
            crate::error::KamalImportError::ErbTemplating { .. }
        ));
        assert!(err.to_string().contains("kamal config"));
    }

    #[test]
    fn commented_out_erb_does_not_trip_the_guard() {
        let yaml = r#"
service: a
image: b/c
# args:
#   RUBY_VERSION: <%= ENV["RBENV_VERSION"] %>
"#;
        assert!(KamalConfig::parse(yaml).is_ok());
    }

    #[test]
    fn missing_required_fields_are_named() {
        let err = KamalConfig::parse("image: only/image\n").unwrap_err();
        assert!(matches!(
            err,
            crate::error::KamalImportError::MissingField { field: "service" }
        ));
        assert!(matches!(
            KamalConfig::parse("").unwrap_err(),
            crate::error::KamalImportError::MissingConfig
        ));
    }

    #[test]
    fn image_reference_omits_default_registry() {
        let yaml = "service: a\nimage: acme/app\n";
        assert_eq!(
            KamalConfig::parse(yaml).unwrap().image_reference(),
            "acme/app:latest"
        );
    }
}
