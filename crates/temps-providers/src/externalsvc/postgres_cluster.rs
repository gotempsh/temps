use anyhow::Result;
use async_trait::async_trait;
use bollard::Docker;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use super::{
    ClusterMemberInfo, ClusterMemberResult, ClusterMemberSpec, ExternalService, RuntimeEnvVar,
    ServiceConfig, ServiceType,
};

/// Default Docker image for pg_auto_failover cluster nodes.
pub(crate) const DEFAULT_CLUSTER_IMAGE: &str = "gotempsh/postgres-ha:18-bookworm-walg";

/// PostgreSQL HA cluster service using pg_auto_failover.
///
/// Topology:
///   - 1 monitor node (lightweight Postgres instance for orchestration)
///   - 1 primary node
///   - N replica nodes (default: 1)
///
/// Each member is a separate Docker container that can run on different worker nodes.
/// pg_autoctl handles replication setup, health monitoring, and automatic failover.
pub struct PostgresClusterService {
    name: String,
    #[allow(dead_code)]
    docker: Arc<Docker>,
}

/// Configuration for a PostgreSQL HA cluster.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PostgresClusterConfig {
    /// Database name
    #[serde(default = "default_database")]
    pub database: String,
    /// Database username
    #[serde(default = "default_username")]
    pub username: String,
    /// Database password (auto-generated if not provided)
    pub password: Option<String>,
    /// Max connections per node
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Number of replicas (default: 1)
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    /// Docker image for cluster nodes
    pub docker_image: Option<String>,
    /// SSL mode between cluster members
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
}

fn default_database() -> String {
    "postgres".to_string()
}
fn default_username() -> String {
    "postgres".to_string()
}
fn default_max_connections() -> u32 {
    100
}
fn default_replicas() -> u32 {
    1
}
fn default_ssl_mode() -> String {
    "prefer".to_string()
}

impl PostgresClusterService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self { name, docker }
    }

    /// Container name for the monitor member.
    fn monitor_container_name(&self) -> String {
        format!("postgres-{}-monitor", self.name)
    }

    /// Container name for a data node member by ordinal.
    fn node_container_name(&self, ordinal: i32) -> String {
        format!("postgres-{}-{}", self.name, ordinal)
    }

    /// Parse cluster config from ServiceConfig parameters.
    fn parse_config(config: &ServiceConfig) -> Result<PostgresClusterConfig> {
        let cluster_config: PostgresClusterConfig =
            serde_json::from_value(config.parameters.clone())
                .map_err(|e| anyhow::anyhow!("Invalid cluster config: {}", e))?;
        Ok(cluster_config)
    }

    /// Build environment variables for the monitor container.
    ///
    /// `monitor_hostname` is the address the monitor advertises to data nodes.
    /// For remote workers this is the WireGuard/private IP; for local it is the container name.
    /// `monitor_port` is the port the monitor listens on (inside the container).
    fn monitor_env(&self, monitor_hostname: &str, monitor_port: u16) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("MONITOR_HOSTNAME".to_string(), monitor_hostname.to_string());
        env.insert("MONITOR_PORT".to_string(), monitor_port.to_string());
        env
    }

    /// Build environment variables for a data node container.
    ///
    /// `monitor_port` is the port the monitor listens on (the mapped host port
    /// when using bridge networking, or the container port with host networking).
    /// `node_port` is the port this node will listen on.
    fn node_env(
        &self,
        config: &PostgresClusterConfig,
        monitor_hostname: &str,
        monitor_port: u16,
        node_hostname: &str,
        node_port: u16,
        node_name: &str,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("NODE_HOSTNAME".to_string(), node_hostname.to_string());
        env.insert("NODE_PORT".to_string(), node_port.to_string());
        env.insert("NODE_NAME".to_string(), node_name.to_string());
        env.insert(
            "MONITOR_URI".to_string(),
            format!(
                "postgresql://autoctl_node@{}:{}/pg_auto_failover",
                monitor_hostname, monitor_port
            ),
        );
        env.insert("POSTGRES_USER".to_string(), config.username.clone());
        env.insert(
            "POSTGRES_PASSWORD".to_string(),
            config
                .password
                .clone()
                .unwrap_or_else(super::postgres::generate_password),
        );
        env.insert("POSTGRES_DB".to_string(), config.database.clone());
        env
    }

    /// Build the startup command for the monitor container.
    ///
    /// The hostname is passed via the `MONITOR_HOSTNAME` environment variable
    /// so that it can be set to the worker node's WireGuard/private address
    /// when the monitor runs on a remote node.
    fn monitor_command(&self) -> Vec<String> {
        // The entrypoint script handles:
        // 1. pg_autoctl create monitor (if not initialized)
        // 2. Remove stale pidfile (prevents "already running with PID 1" on restart)
        // 3. pg_autoctl run
        //
        // Runs as the `postgres` user because pg_ctl refuses to run as root.
        vec![
            "bash".to_string(),
            "-c".to_string(),
            [
                "PGDATA=/var/lib/postgresql/monitor",
                "chown -R postgres:postgres /var/lib/postgresql",
                "if [ ! -f \"$PGDATA/pg_autoctl.cfg\" ]; then",
                "  gosu postgres pg_autoctl create monitor \\",
                "    --pgdata \"$PGDATA\" \\",
                "    --pgport \"$MONITOR_PORT\" \\",
                "    --hostname \"$MONITOR_HOSTNAME\" \\",
                "    --auth trust \\",
                "    --ssl-self-signed;",
                "fi",
                // After creation (or on restart), ensure pg_hba.conf allows
                // autoctl_node connections via trust over the network.
                // --ssl-self-signed sets cert auth for SSL connections, but
                // data nodes need trust for the initial registration handshake
                // before pg_autoctl has issued them client certificates.
                "HBA=\"$PGDATA/pg_hba.conf\"",
                "if ! grep -q 'autoctl_node.*0\\.0\\.0\\.0/0' \"$HBA\" 2>/dev/null; then",
                "  echo 'hostssl pg_auto_failover autoctl_node 0.0.0.0/0 trust' >> \"$HBA\"",
                "  echo 'hostssl pg_auto_failover autoctl_node ::/0 trust' >> \"$HBA\"",
                "  gosu postgres pg_ctl reload -D \"$PGDATA\" 2>/dev/null || true",
                "fi",
                "rm -f /tmp/pg_autoctl/*.pid /tmp/pg_autoctl/*/*.pid",
                "exec gosu postgres pg_autoctl run --pgdata \"$PGDATA\"",
            ]
            .join("\n"),
        ]
    }

    /// Build the startup command for a data node container.
    fn node_command(&self) -> Vec<String> {
        // The entrypoint script handles:
        // 1. Launch a background HBA patcher that waits for pg_hba.conf to appear
        //    and immediately adds trust entries for replication connections.
        //    This MUST run concurrently with pg_autoctl create because the FSM
        //    transition (primary → catchingup) happens inside `create` before
        //    the command returns — sequential patching is too late.
        // 2. pg_autoctl create postgres (if not initialized) — connects to monitor
        // 3. Remove stale pidfile (prevents "already running with PID 1" on restart)
        // 4. pg_autoctl run — keeps running, handles replication and failover
        //
        // Runs as the `postgres` user because pg_ctl refuses to run as root.
        vec![
            "bash".to_string(),
            "-c".to_string(),
            [
                "PGDATA=/var/lib/postgresql/pgdata",
                "chown -R postgres:postgres /var/lib/postgresql",
                // Background HBA patcher: polls for pg_hba.conf and patches it
                // as soon as it exists. Needed because --ssl-self-signed sets
                // cert auth, but remote cluster members need trust auth for
                // pgautofailover_replicator (replication) and autoctl_node
                // (monitor communication) before certificates are exchanged.
                "(",
                "  while true; do",
                "    HBA=\"$PGDATA/pg_hba.conf\"",
                "    if [ -f \"$HBA\" ]; then",
                "      if ! grep -q 'pgautofailover_replicator.*0\\.0\\.0\\.0/0' \"$HBA\" 2>/dev/null; then",
                "        echo 'hostssl replication pgautofailover_replicator 0.0.0.0/0 trust' >> \"$HBA\"",
                "        echo 'hostssl replication pgautofailover_replicator ::/0 trust' >> \"$HBA\"",
                "        echo 'host replication pgautofailover_replicator 0.0.0.0/0 trust' >> \"$HBA\"",
                "        echo 'host replication pgautofailover_replicator ::/0 trust' >> \"$HBA\"",
                "        echo 'hostssl all pgautofailover_replicator 0.0.0.0/0 trust' >> \"$HBA\"",
                "        echo 'hostssl all pgautofailover_replicator ::/0 trust' >> \"$HBA\"",
                "        echo 'host all pgautofailover_replicator 0.0.0.0/0 trust' >> \"$HBA\"",
                "        echo 'host all pgautofailover_replicator ::/0 trust' >> \"$HBA\"",
                "        gosu postgres pg_ctl reload -D \"$PGDATA\" 2>/dev/null || true",
                "      fi",
                // Application + tooling user access (ADR-011 follow-up):
                // pg_auto_failover only auto-generates pg_hba rules for
                // its infrastructure users (pgautofailover_replicator,
                // pgautofailover_monitor) and a `<self>:<self> trust`
                // line that lets a node connect to itself. Every other
                // caller — sibling cluster members, control-plane health
                // probes, the Browse Data UI, app containers on the
                // overlay, the auto-provisioned `temps_explorer`
                // read-only user, any future per-tenant role we add —
                // gets "no pg_hba.conf entry for host X, user Y" until
                // we open it explicitly.
                //
                // We add ONE catch-all md5 rule rather than a per-user
                // entry so future roles work without a code change.
                // Auth is still password-protected; the rule just
                // says "if the role exists and the password matches,
                // let it in from anywhere on the network the cluster
                // already trusts".
                //
                // Order matters in pg_hba — the trust rules above this
                // block (replicator + auto-generated monitor) match
                // first, so infrastructure users skip md5 and keep
                // their cert/trust auth.
                "      if ! grep -q '^host all all 0\\.0\\.0\\.0/0 md5' \"$HBA\" 2>/dev/null; then",
                "        echo 'hostssl all all 0.0.0.0/0 md5' >> \"$HBA\"",
                "        echo 'hostssl all all ::/0 md5' >> \"$HBA\"",
                "        echo 'host all all 0.0.0.0/0 md5' >> \"$HBA\"",
                "        echo 'host all all ::/0 md5' >> \"$HBA\"",
                "        gosu postgres pg_ctl reload -D \"$PGDATA\" 2>/dev/null || true",
                "      fi",
                "      break",
                "    fi",
                "    sleep 0.5",
                "  done",
                ") &",
                // Separate background loop: ensure the configured app user
                // exists with the configured password, idempotently.
                //
                // Why a separate loop: pg_auto_failover invokes initdb with
                // `--auth trust` which leaves the superuser without a
                // password, so external md5 auth always fails until we
                // ALTER it. We can't run this synchronously at script
                // top because Postgres isn't listening yet; we can't
                // batch it with the HBA patcher (which exits on first
                // patch) because Postgres might come up *after* the HBA
                // patcher finishes. So this is its own loop that retries
                // every 2s until the ALTER succeeds, then exits.
                //
                // The script writes the SQL to a tempfile rather than
                // -c'ing it inline so embedded $$ and quotes don't need
                // round-trip escaping through the bash heredoc. We
                // chmod the file 644 so `gosu postgres psql` (which
                // drops to the postgres user) can read it — without
                // this it lives as 600 root:root, every retry hits
                // EACCES, the loop times out and the password never
                // gets ALTERed, breaking auth for every external
                // caller including Browse Data.
                "(",
                "  SQL_FILE=$(mktemp /tmp/temps-app-user-XXXX.sql)",
                "  cat > \"$SQL_FILE\" <<SQL_EOF",
                "DO \\$\\$",
                "BEGIN",
                "  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${POSTGRES_USER}') THEN",
                "    CREATE ROLE \"${POSTGRES_USER}\" LOGIN SUPERUSER PASSWORD '${POSTGRES_PASSWORD}';",
                "  ELSE",
                "    ALTER ROLE \"${POSTGRES_USER}\" WITH LOGIN SUPERUSER PASSWORD '${POSTGRES_PASSWORD}';",
                "  END IF;",
                "END",
                "\\$\\$;",
                "SELECT 'CREATE DATABASE \"${POSTGRES_DB}\" OWNER \"${POSTGRES_USER}\"'",
                "WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${POSTGRES_DB}')\\gexec",
                "SQL_EOF",
                "  chmod 644 \"$SQL_FILE\"",
                "  for _ in $(seq 1 60); do",
                "    if gosu postgres psql -p \"$NODE_PORT\" -d postgres -v ON_ERROR_STOP=1 -f \"$SQL_FILE\" >/dev/null 2>&1; then",
                "      rm -f \"$SQL_FILE\"",
                "      exit 0",
                "    fi",
                "    sleep 2",
                "  done",
                "  rm -f \"$SQL_FILE\"",
                ") &",
                "if [ ! -f \"$PGDATA/pg_autoctl.cfg\" ]; then",
                "  gosu postgres pg_autoctl create postgres \\",
                "    --pgdata \"$PGDATA\" \\",
                "    --pgport \"$NODE_PORT\" \\",
                "    --hostname \"$NODE_HOSTNAME\" \\",
                "    --name \"$NODE_NAME\" \\",
                "    --auth trust \\",
                "    --ssl-self-signed \\",
                "    --monitor \"$MONITOR_URI\";",
                "fi",
                "rm -f /tmp/pg_autoctl/*.pid /tmp/pg_autoctl/*/*.pid",
                "exec gosu postgres pg_autoctl run --pgdata \"$PGDATA\"",
            ]
            .join("\n"),
        ]
    }
}

#[async_trait]
impl ExternalService for PostgresClusterService {
    async fn init(&self, _config: ServiceConfig) -> Result<HashMap<String, String>> {
        // Cluster services use init_cluster instead
        Err(anyhow::anyhow!(
            "Use init_cluster for PostgresClusterService — standalone init not supported"
        ))
    }

    async fn health_check(&self) -> Result<bool> {
        // Cluster health is checked per-member by the ExternalServiceManager
        Ok(true)
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Postgres
    }

    fn get_name(&self) -> String {
        format!("postgres-cluster-{}", self.name)
    }

    fn get_connection_info(&self) -> Result<String> {
        // Connection info is generated from cluster members by the manager
        Ok(format!(
            "postgres-cluster-{} (use cluster endpoint)",
            self.name
        ))
    }

    async fn cleanup(&self) -> Result<()> {
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        let schema = schemars::schema_for!(PostgresClusterConfig);
        serde_json::to_value(schema).ok()
    }

    async fn start(&self) -> Result<()> {
        // Cluster start is managed per-member
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Cluster stop is managed per-member
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        // Cluster removal is managed per-member
        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();
        let user = parameters.get("username").cloned().unwrap_or_default();
        let password = parameters.get("password").cloned().unwrap_or_default();
        let database = parameters.get("database").cloned().unwrap_or_default();

        // For clusters, connection info includes all data node hosts
        env.insert("POSTGRES_USER".to_string(), user);
        env.insert("POSTGRES_PASSWORD".to_string(), password);
        env.insert("POSTGRES_DATABASE".to_string(), database);

        Ok(env)
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        self.get_environment_variables(parameters)
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "POSTGRES_URL".to_string(),
                description: "Multi-host PostgreSQL connection string with failover support"
                    .to_string(),
                example: "postgresql://user:pass@host1:5432,host2:5432/db?target_session_attrs=read-write".to_string(),
                sensitive: true,
            },
            RuntimeEnvVar {
                name: "POSTGRES_HOST".to_string(),
                description: "Comma-separated list of PostgreSQL cluster hosts".to_string(),
                example: "host1,host2".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "POSTGRES_PORT".to_string(),
                description: "PostgreSQL port".to_string(),
                example: "5432".to_string(),
                sensitive: false,
            },
        ]
    }

    fn get_local_address(&self, _service_config: ServiceConfig) -> Result<String> {
        Ok("localhost:5432".to_string())
    }

    fn get_effective_address(&self, _service_config: ServiceConfig) -> Result<(String, String)> {
        // For clusters, the effective address is the primary — but this is dynamic
        Ok((self.monitor_container_name(), "5432".to_string()))
    }

    fn get_docker_container_name(&self) -> String {
        self.monitor_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        "5432".to_string()
    }

    // -----------------------------------------------------------------------
    // Cluster-specific methods
    // -----------------------------------------------------------------------

    fn supports_cluster(&self) -> bool {
        true
    }

    fn valid_cluster_roles(&self) -> Vec<&'static str> {
        // Source of truth: the ClusterRole enum. Order matches insertion
        // order callers expect (monitor first, then data nodes).
        vec![
            super::ClusterRole::Monitor.as_str(),
            super::ClusterRole::Primary.as_str(),
            super::ClusterRole::Replica.as_str(),
        ]
    }

    async fn init_cluster(
        &self,
        config: ServiceConfig,
        members: Vec<ClusterMemberSpec>,
    ) -> Result<Vec<ClusterMemberResult>> {
        let _cluster_config = Self::parse_config(&config)?;
        // Always use the HA image for cluster members — the standalone
        // postgres-walg image does not contain pg_auto_failover / pg_autoctl.
        let image = DEFAULT_CLUSTER_IMAGE;

        info!(
            "Initializing PostgreSQL HA cluster '{}' with {} members (image: {})",
            self.name,
            members.len(),
            image
        );

        let mut results = Vec::new();

        // Find the monitor member — must be initialized first
        let monitor = members
            .iter()
            .find(|m| m.role == "monitor")
            .ok_or_else(|| anyhow::anyhow!("Cluster must have exactly one monitor member"))?;

        let _monitor_hostname = monitor
            .hostname
            .as_deref()
            .unwrap_or(&self.monitor_container_name());

        // Create monitor container
        let monitor_container_name = self.monitor_container_name();
        info!("Creating monitor container: {}", monitor_container_name);

        let monitor_result = ClusterMemberResult {
            ordinal: monitor.ordinal,
            role: "monitor".to_string(),
            container_id: String::new(), // Filled by the manager after remote/local creation
            container_name: monitor_container_name.clone(),
            port: Some(5432),
            status: "provisioning".to_string(),
        };
        results.push(monitor_result);

        // Create data node containers (primary first, then replicas)
        // pg_auto_failover automatically assigns primary to the first registered node
        let mut data_nodes: Vec<&ClusterMemberSpec> =
            members.iter().filter(|m| m.role != "monitor").collect();
        // Sort: primary first, then replicas by ordinal
        data_nodes.sort_by(|a, b| {
            let a_is_primary = if a.role == "primary" { 0 } else { 1 };
            let b_is_primary = if b.role == "primary" { 0 } else { 1 };
            a_is_primary
                .cmp(&b_is_primary)
                .then(a.ordinal.cmp(&b.ordinal))
        });

        for node in &data_nodes {
            let container_name = self.node_container_name(node.ordinal);
            info!(
                "Creating data node container: {} (role: {}, ordinal: {})",
                container_name, node.role, node.ordinal
            );

            let node_result = ClusterMemberResult {
                ordinal: node.ordinal,
                role: node.role.clone(),
                container_id: String::new(),
                container_name,
                port: Some(5432),
                status: "provisioning".to_string(),
            };
            results.push(node_result);
        }

        Ok(results)
    }

    fn cluster_connection_string(
        &self,
        members: &[ClusterMemberInfo],
        config: &ServiceConfig,
    ) -> Result<String> {
        let cluster_config = Self::parse_config(config)?;

        let data_nodes: Vec<&ClusterMemberInfo> = members
            .iter()
            .filter(|m| m.role != "monitor" && m.status == "running")
            .collect();

        if data_nodes.is_empty() {
            return Err(anyhow::anyhow!("No running data nodes in cluster"));
        }

        let password = cluster_config.password.unwrap_or_default();
        let encoded_password = urlencoding::encode(&password);

        // ADR-011: when every data node carries an FQDN that resolves via
        // the per-node DNS resolver (`*.temps.local`), collapse the multi-host
        // libpq workaround into a single VIP. Failover then becomes
        // a DNS-records flip — apps' next connection (or libpq's automatic
        // retry) lands on whatever the current primary is, with no
        // redeploy. The records are owned by the per-cluster reconciler
        // (Phase 4) and refreshed every few seconds.
        let all_fqdn = data_nodes
            .iter()
            .all(|m| m.hostname.ends_with(".temps.local"));

        let connection_string = if all_fqdn {
            // Use the per-service VIP. Picks any healthy data node by
            // multi-A round-robin; libpq + target_session_attrs lands
            // writes on the primary.
            //
            // Port: every data node listens on the same container port, so
            // we read it off the first member.
            let port = data_nodes[0].port;
            format!(
                "postgresql://{}:{}@{}.temps.local:{}/{}?target_session_attrs=read-write",
                cluster_config.username, encoded_password, self.name, port, cluster_config.database,
            )
        } else {
            // Legacy / single-host fallback: emit the explicit multi-host
            // libpq string. Used when DNS isn't wired (no `temps.local`
            // suffix on member hostnames) — typically integration tests
            // or pre-DNS deployments.
            let hosts: Vec<String> = data_nodes
                .iter()
                .map(|n| format!("{}:{}", n.hostname, n.port))
                .collect();
            format!(
                "postgresql://{}:{}@{}/{}?target_session_attrs=read-write",
                cluster_config.username,
                encoded_password,
                hosts.join(","),
                cluster_config.database,
            )
        };

        Ok(connection_string)
    }

    fn get_cluster_docker_image(&self) -> (String, String) {
        (DEFAULT_CLUSTER_IMAGE.to_string(), "18-bookworm".to_string())
    }
}

/// Build `RemoteServiceCreateParams`-compatible data for a cluster member.
/// This is called by `ExternalServiceManager` when dispatching member creation
/// to remote worker nodes via the agent API.
#[derive(Clone)]
pub struct ClusterMemberCreateParams {
    pub container_name: String,
    pub image: String,
    pub environment: HashMap<String, String>,
    pub command: Option<Vec<String>>,
    pub container_port: u16,
    pub volume_path: String,
}

impl PostgresClusterService {
    /// Build creation parameters for a specific cluster member.
    ///
    /// * `monitor_hostname` — address the monitor advertises (host IP or container name)
    /// * `monitor_port` — port the monitor listens on (the host-mapped port)
    /// * `member_port` — port this member will listen on inside its container
    ///
    /// The manager uses these to create containers locally or via the agent.
    pub fn build_member_params(
        &self,
        member: &ClusterMemberSpec,
        config: &PostgresClusterConfig,
        monitor_hostname: &str,
        monitor_port: u16,
        member_port: u16,
    ) -> ClusterMemberCreateParams {
        use std::str::FromStr;
        // Unknown roles fall through the wildcard arm (treated as a data
        // node) — matches old behaviour. Validation at the create-service
        // boundary is what actually rejects garbage roles.
        let role = super::ClusterRole::from_str(&member.role).ok();
        match role {
            Some(super::ClusterRole::Monitor) => ClusterMemberCreateParams {
                container_name: self.monitor_container_name(),
                // Always use the HA image — parameter_strategies may fill in the
                // standalone postgres-walg image which lacks pg_autoctl.
                image: DEFAULT_CLUSTER_IMAGE.to_string(),
                environment: self.monitor_env(monitor_hostname, member_port),
                command: Some(self.monitor_command()),
                container_port: member_port,
                volume_path: "/var/lib/postgresql".to_string(),
            },
            // primary | replica | unknown → data-node setup; pg_auto_failover
            // elects which is which at runtime, so we only need one branch.
            _ => {
                let fallback_hostname = self.node_container_name(member.ordinal);
                let node_hostname = member.hostname.as_deref().unwrap_or(&fallback_hostname);
                // Register the pg_autoctl node under the same name as the
                // docker container, so the Cluster Health view (which is
                // populated from pg_autoctl) matches the Cluster Members
                // view (populated from `service_members.container_name`)
                // line-for-line. The previous `node-{ordinal}` scheme
                // collided when an ordinal was reused after a delete: the
                // monitor kept the old "node-2" identity while temps was
                // already calling the new container "postgres-e3m4-N".
                let node_name = self.node_container_name(member.ordinal);

                ClusterMemberCreateParams {
                    container_name: self.node_container_name(member.ordinal),
                    // Always use the HA image — see monitor comment above.
                    image: DEFAULT_CLUSTER_IMAGE.to_string(),
                    environment: self.node_env(
                        config,
                        monitor_hostname,
                        monitor_port,
                        node_hostname,
                        member_port,
                        &node_name,
                    ),
                    command: Some(self.node_command()),
                    container_port: member_port,
                    volume_path: "/var/lib/postgresql".to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_naming() {
        let service = PostgresClusterService::new(
            "my-db".to_string(),
            Arc::new(Docker::connect_with_defaults().unwrap_or_else(|_| {
                // Fallback for tests without Docker
                Docker::connect_with_local_defaults().unwrap()
            })),
        );

        assert_eq!(service.monitor_container_name(), "postgres-my-db-monitor");
        assert_eq!(service.node_container_name(1), "postgres-my-db-1");
        assert_eq!(service.node_container_name(2), "postgres-my-db-2");
    }

    #[test]
    fn test_valid_cluster_roles() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));
        assert!(service.supports_cluster());
        assert_eq!(
            service.valid_cluster_roles(),
            vec!["monitor", "primary", "replica"]
        );
    }

    #[test]
    fn test_parse_cluster_config_defaults() {
        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({}),
        };
        let cluster_config = PostgresClusterService::parse_config(&config).unwrap();
        assert_eq!(cluster_config.database, "postgres");
        assert_eq!(cluster_config.username, "postgres");
        assert_eq!(cluster_config.max_connections, 100);
        assert_eq!(cluster_config.replicas, 1);
    }

    #[test]
    fn test_parse_cluster_config_custom() {
        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "database": "myapp",
                "username": "admin",
                "password": "secret123",
                "replicas": 2,
                "max_connections": 200
            }),
        };
        let cluster_config = PostgresClusterService::parse_config(&config).unwrap();
        assert_eq!(cluster_config.database, "myapp");
        assert_eq!(cluster_config.username, "admin");
        assert_eq!(cluster_config.password, Some("secret123".to_string()));
        assert_eq!(cluster_config.replicas, 2);
        assert_eq!(cluster_config.max_connections, 200);
    }

    #[test]
    fn test_cluster_connection_string() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));

        let members = vec![
            ClusterMemberInfo {
                role: "monitor".to_string(),
                hostname: "10.100.0.1".to_string(),
                port: 5432,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "primary".to_string(),
                hostname: "10.100.0.2".to_string(),
                port: 5432,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "replica".to_string(),
                hostname: "10.100.0.3".to_string(),
                port: 5432,
                status: "running".to_string(),
            },
        ];

        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "database": "myapp",
                "username": "admin",
                "password": "secret"
            }),
        };

        let conn_str = service
            .cluster_connection_string(&members, &config)
            .unwrap();

        // Monitor should NOT be in the connection string
        assert!(!conn_str.contains("10.100.0.1"));
        // Both data nodes should be present
        assert!(conn_str.contains("10.100.0.2:5432"));
        assert!(conn_str.contains("10.100.0.3:5432"));
        // Should have multi-host format with failover
        assert!(conn_str.contains("target_session_attrs=read-write"));
        assert!(conn_str.starts_with("postgresql://admin:secret@"));
    }

    #[test]
    fn test_monitor_command_contains_ssl() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));
        let cmd = service.monitor_command();
        let script = &cmd[2];
        assert!(script.contains("gosu postgres pg_autoctl create monitor"));
        assert!(script.contains("--ssl-self-signed"));
        assert!(script.contains("--pgport \"$MONITOR_PORT\""));
        assert!(script.contains("gosu postgres pg_autoctl run"));
        assert!(script.contains("$MONITOR_HOSTNAME"));
        assert!(script.contains("chown -R postgres:postgres"));
        // Must patch pg_hba.conf to allow autoctl_node trust auth for node registration
        assert!(script.contains("autoctl_node 0.0.0.0/0 trust"));
        assert!(script.contains("hostssl pg_auto_failover autoctl_node 0.0.0.0/0 trust"));
    }

    #[test]
    fn test_node_command_contains_monitor_uri() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));
        let cmd = service.node_command();
        let script = &cmd[2];
        assert!(script.contains("gosu postgres pg_autoctl create postgres"));
        assert!(script.contains("--pgport \"$NODE_PORT\""));
        assert!(script.contains("$MONITOR_URI"));
        assert!(script.contains("$NODE_HOSTNAME"));
        assert!(script.contains("--ssl-self-signed"));
        assert!(script.contains("chown -R postgres:postgres"));
    }

    #[test]
    fn test_build_member_params_monitor() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("my-db".to_string(), Arc::new(docker));
        let config = PostgresClusterConfig {
            database: "postgres".to_string(),
            username: "postgres".to_string(),
            password: Some("pass".to_string()),
            max_connections: 100,
            replicas: 1,
            docker_image: None,
            ssl_mode: "prefer".to_string(),
        };

        let spec = ClusterMemberSpec {
            role: "monitor".to_string(),
            node_id: Some(1),
            ordinal: 0,
            hostname: Some("10.100.0.1".to_string()),
        };

        let params = service.build_member_params(&spec, &config, "10.100.0.1", 6100, 6100);
        assert_eq!(params.container_name, "postgres-my-db-monitor");
        assert_eq!(params.container_port, 6100);
        assert_eq!(params.image, DEFAULT_CLUSTER_IMAGE);
        // Monitor env should contain the hostname and port for pg_autoctl advertisement
        assert_eq!(
            params.environment.get("MONITOR_HOSTNAME").unwrap(),
            "10.100.0.1"
        );
        assert_eq!(params.environment.get("MONITOR_PORT").unwrap(), "6100");
    }

    #[test]
    fn test_build_member_params_data_node() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("my-db".to_string(), Arc::new(docker));
        let config = PostgresClusterConfig {
            database: "myapp".to_string(),
            username: "admin".to_string(),
            password: Some("secret".to_string()),
            max_connections: 200,
            replicas: 1,
            docker_image: None,
            ssl_mode: "prefer".to_string(),
        };

        let spec = ClusterMemberSpec {
            role: "primary".to_string(),
            node_id: Some(2),
            ordinal: 1,
            hostname: Some("10.100.0.2".to_string()),
        };

        let params = service.build_member_params(&spec, &config, "10.100.0.1", 6100, 6101);
        assert_eq!(params.container_name, "postgres-my-db-1");
        assert_eq!(params.container_port, 6101);
        assert_eq!(
            params.environment.get("MONITOR_URI").unwrap(),
            "postgresql://autoctl_node@10.100.0.1:6100/pg_auto_failover"
        );
        assert_eq!(
            params.environment.get("NODE_HOSTNAME").unwrap(),
            "10.100.0.2"
        );
        assert_eq!(params.environment.get("NODE_PORT").unwrap(), "6101");
        assert_eq!(params.environment.get("POSTGRES_USER").unwrap(), "admin");
        assert_eq!(params.environment.get("POSTGRES_DB").unwrap(), "myapp");
    }
}
