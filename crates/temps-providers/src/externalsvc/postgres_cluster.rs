// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Parameter key holding the `autoctl_node` password (data node -> monitor).
pub const AUTOCTL_NODE_PASSWORD_PARAM: &str = "autoctl_node_password";
/// Parameter key holding the `pgautofailover_replicator` password
/// (standby -> primary streaming replication, `pg_rewind`, base backups).
pub const REPLICATION_PASSWORD_PARAM: &str = "replication_password";

/// Length of each generated cluster-internal secret.
const CLUSTER_SECRET_LEN: usize = 32;

/// Credentials pg_auto_failover's infrastructure roles authenticate with.
///
/// Clusters used to admit `autoctl_node` (monitor) and
/// `pgautofailover_replicator` (data nodes) with `trust` from `0.0.0.0/0`,
/// so any container that could reach a member port — including other
/// tenants' applications on the shared Docker network — could register or
/// drive failover on the monitor, or stream a physical copy of every
/// database. Both roles now authenticate with SCRAM-SHA-256 using these
/// generated secrets, which are stored only inside the service's encrypted
/// parameter blob (their keys end in `_password`, so API responses mask
/// them).
///
/// Secrets are ASCII alphanumeric by construction and validated as such on
/// load, so they can be embedded in a libpq URI and in the member shell
/// scripts without quoting or escaping.
#[derive(Clone, PartialEq, Eq)]
pub struct ClusterAuthSecrets {
    pub autoctl_node_password: String,
    pub replication_password: String,
}

impl std::fmt::Debug for ClusterAuthSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterAuthSecrets").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterAuthError {
    #[error("Cluster parameter '{key}' must be a non-empty alphanumeric string")]
    InvalidParameter { key: String },
    #[error("Both cluster infrastructure credentials must be present together")]
    IncompleteCredentials,
    #[error("Cluster service {service_id} not found")]
    ServiceNotFound { service_id: i32 },
    #[error("Failed to load cluster service {service_id}: {source}")]
    Database {
        service_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Failed to decrypt cluster service {service_id} configuration: {source}")]
    Decryption {
        service_id: i32,
        #[source]
        source: anyhow::Error,
    },
    #[error("Failed to parse cluster service {service_id} configuration: {source}")]
    Configuration {
        service_id: i32,
        #[source]
        source: serde_json::Error,
    },
}

impl ClusterAuthSecrets {
    /// Generate a fresh pair of secrets.
    pub fn generate() -> Self {
        Self {
            autoctl_node_password: generate_cluster_secret(),
            replication_password: generate_cluster_secret(),
        }
    }

    /// Read the secrets from a cluster's decrypted parameters.
    ///
    /// Returns `Ok(None)` when both keys are absent (a cluster created before
    /// SCRAM auth), and an error when a stored value is not a non-empty
    /// alphanumeric string — which would mean the blob was edited by hand
    /// and must not be spliced into a URI or script.
    pub fn from_parameters(
        parameters: &HashMap<String, serde_json::Value>,
    ) -> std::result::Result<Option<Self>, ClusterAuthError> {
        let read = |key: &str| -> std::result::Result<Option<String>, ClusterAuthError> {
            match parameters.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(value)) if is_valid_cluster_secret(value) => {
                    Ok(Some(value.clone()))
                }
                Some(_) => Err(ClusterAuthError::InvalidParameter {
                    key: key.to_string(),
                }),
            }
        };
        match (
            read(AUTOCTL_NODE_PASSWORD_PARAM)?,
            read(REPLICATION_PASSWORD_PARAM)?,
        ) {
            (Some(autoctl_node_password), Some(replication_password)) => Ok(Some(Self {
                autoctl_node_password,
                replication_password,
            })),
            (None, None) => Ok(None),
            _ => Err(ClusterAuthError::IncompleteCredentials),
        }
    }

    /// Write the secrets into a parameter map (to be encrypted and stored).
    pub fn insert_into(&self, parameters: &mut HashMap<String, serde_json::Value>) {
        parameters.insert(
            AUTOCTL_NODE_PASSWORD_PARAM.to_string(),
            serde_json::Value::String(self.autoctl_node_password.clone()),
        );
        parameters.insert(
            REPLICATION_PASSWORD_PARAM.to_string(),
            serde_json::Value::String(self.replication_password.clone()),
        );
    }

    /// Environment for [`PostgresClusterService::auth_hardening_command`]
    /// and the member containers.
    pub fn env(&self) -> HashMap<String, String> {
        HashMap::from([
            (
                "AUTOCTL_NODE_PASSWORD".to_string(),
                self.autoctl_node_password.clone(),
            ),
            (
                "REPLICATION_PASSWORD".to_string(),
                self.replication_password.clone(),
            ),
        ])
    }
}

/// Load a cluster's SCRAM secrets from its encrypted parameter blob.
///
/// `Ok(None)` for a cluster that has not been upgraded yet (its monitor still
/// admits `autoctl_node` without a password). Errors carry the service id.
pub async fn load_cluster_auth_secrets(
    db: &sea_orm::DatabaseConnection,
    encryption: &temps_core::EncryptionService,
    service_id: i32,
) -> std::result::Result<Option<ClusterAuthSecrets>, ClusterAuthError> {
    use sea_orm::EntityTrait;
    let service = temps_entities::external_services::Entity::find_by_id(service_id)
        .one(db)
        .await
        .map_err(|source| ClusterAuthError::Database { service_id, source })?
        .ok_or(ClusterAuthError::ServiceNotFound { service_id })?;
    let Some(encrypted) = service.config else {
        return Ok(None);
    };
    let decrypted = encryption
        .decrypt_string(&encrypted)
        .map_err(|source| ClusterAuthError::Decryption { service_id, source })?;
    let parameters: HashMap<String, serde_json::Value> = serde_json::from_str(&decrypted)
        .map_err(|source| ClusterAuthError::Configuration { service_id, source })?;
    ClusterAuthSecrets::from_parameters(&parameters)
}

/// libpq connection string the control plane uses to read the monitor
/// (`pgautofailover.node`).
///
/// `sslmode=require` keeps the socket encrypted even though the monitor's
/// certificate is self-signed. With SCRAM a forged endpoint observes only a
/// challenge-response exchange, never the password itself. `auth` is `None`
/// only for clusters that have not been upgraded yet, whose monitor still
/// admits `autoctl_node` without a password.
pub fn monitor_connection_string(
    host: &str,
    port: i32,
    auth: Option<&ClusterAuthSecrets>,
) -> String {
    let mut conn = format!(
        "host={host} port={port} user=autoctl_node dbname=pg_auto_failover \
         sslmode=require connect_timeout=3"
    );
    if let Some(auth) = auth {
        // Alphanumeric by construction: no libpq quoting needed.
        conn.push_str(" password=");
        conn.push_str(&auth.autoctl_node_password);
    }
    conn
}

/// `host` as it must appear in a `postgresql://` URI authority: an IPv6
/// literal is bracketed (RFC 3986), anything else is unchanged.
///
/// Cluster members advertise bare addresses (pg_autoctl `--hostname`, Docker
/// port bindings and libpq `host=` all take `fd00::10` as is), but in a URI
/// `user@fd00::10:6030` is unparsable: a data member given that monitor URI
/// could never register.
pub(crate) fn uri_host(host: &str) -> std::borrow::Cow<'_, str> {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        std::borrow::Cow::Owned(format!("[{host}]"))
    } else {
        std::borrow::Cow::Borrowed(host)
    }
}

fn generate_cluster_secret() -> String {
    use rand::{distr::Alphanumeric, RngExt};
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(CLUSTER_SECRET_LEN)
        .map(char::from)
        .collect()
}

fn is_valid_cluster_secret(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// SCRAM authentication for pg_auto_failover's infrastructure roles
// ---------------------------------------------------------------------------
//
// Each fragment is idempotent and shared by SCRAM-native member creation
// and the in-place upgrade the control plane runs against existing clusters.
// The upgrade uses `PostgresClusterService::auth_upgrade_command`; a restarted
// monitor preserves its volume's authentication until every peer is prepared.
// The upgrade runs them in
// the order of `AuthUpgradeStep::ORDER`: every password is distributed while
// the legacy `trust` rules still admit everyone, and only then are the rules
// switched to SCRAM, so no connection that is needed for replication or
// failover ever presents without a password.

/// Monitor, prepare: record `scram-sha-256` as the auth method pg_autoctl
/// uses for rules it adds later, and set the `autoctl_node` password once
/// Postgres accepts connections. Expects `PGDATA`, `MONITOR_PORT` and
/// `AUTOCTL_NODE_PASSWORD`. Starts a background retry loop.
const MONITOR_PREPARE_SNIPPET: &str = r#"gosu postgres pg_autoctl config set --pgdata "$PGDATA" postgresql.auth_method scram-sha-256 >/dev/null 2>&1 || true
(
  for _ in $(seq 1 120); do
    if printf '%s\n' "ALTER ROLE autoctl_node PASSWORD :'pw';" \
      | gosu postgres psql -X -q -p "$MONITOR_PORT" -d pg_auto_failover -v ON_ERROR_STOP=1 -v pw="$AUTOCTL_NODE_PASSWORD" >/dev/null 2>&1; then
      rm -f "${PGDATA%/*}/.temps-monitor-scram-initializing" || exit 1
      exit 0
    fi
    sleep 1
  done
  exit 1
) &"#;

/// Monitor, enforce: rewrite every `trust` rule for `autoctl_node` (the
/// legacy `0.0.0.0/0` rules, and pg_auto_failover's own LAN rule on clusters
/// created with `--auth trust`) to `scram-sha-256`, and keep one
/// `0.0.0.0/0` / `::/0` SCRAM rule so data nodes on other hosts (WireGuard /
/// private addresses outside the detected LAN) can still register. Member
/// ports are only ever published on loopback or the node's private address.
/// Expects `PGDATA`.
const MONITOR_ENFORCE_SNIPPET: &str = r#"HBA="$PGDATA/pg_hba.conf"
if [ -f "$HBA" ]; then
  sed -i -E '/^[[:space:]]*host(ssl)?[[:space:]].*autoctl_node/ s/[[:space:]]trust([[:space:]]|$)/ scram-sha-256\1/' "$HBA"
  if ! grep -q 'autoctl_node.*0\.0\.0\.0/0' "$HBA" 2>/dev/null; then
    echo 'hostssl pg_auto_failover autoctl_node 0.0.0.0/0 scram-sha-256' >> "$HBA"
    echo 'hostssl pg_auto_failover autoctl_node ::/0 scram-sha-256' >> "$HBA"
  fi
  # PostgreSQL uses the first matching HBA rule. Keep permanent SCRAM
  # guards ahead of peer rules a legacy keeper may regenerate later.
  GUARDED_HBA=$(mktemp "$PGDATA/.temps-hba.XXXXXX")
  for CIDR in 0.0.0.0/0 ::/0; do
    for DATABASE in all; do
      printf 'hostnossl %s autoctl_node %s reject # Temps infrastructure authentication\n' "$DATABASE" "$CIDR"
      printf 'hostssl %s autoctl_node %s scram-sha-256 # Temps infrastructure authentication\n' "$DATABASE" "$CIDR"
    done
  done > "$GUARDED_HBA"
  sed '/# Temps infrastructure authentication$/d' "$HBA" >> "$GUARDED_HBA"
  mv "$GUARDED_HBA" "$HBA"
  chown postgres:postgres "$HBA"
  gosu postgres pg_ctl reload -D "$PGDATA" >/dev/null 2>&1 || true
fi"#;

/// Data node: write `~postgres/.pgpass` for `pgautofailover_replicator`.
/// libpq reads it for base backups, `pg_rewind` and the WAL receiver, so the
/// password never appears in a connection string. Must run before
/// `pg_autoctl create postgres`, which clones a standby from the primary.
/// Expects `REPLICATION_PASSWORD`.
const NODE_PGPASS_SNIPPET: &str = r#"PGPASS_FILE=/var/lib/postgresql/.pgpass
printf '*:*:*:pgautofailover_replicator:%s\n*:*:*:autoctl_node:%s\n' "$REPLICATION_PASSWORD" "$AUTOCTL_NODE_PASSWORD" > "$PGPASS_FILE"
chown postgres:postgres "$PGPASS_FILE"
chmod 600 "$PGPASS_FILE""#;

/// Data node, prepare: store the replication password and the SCRAM auth
/// method in pg_autoctl's config, inject the `autoctl_node` password into
/// the configured monitor URI (clusters created before SCRAM have none), and
/// set the replicator role password on whichever node is primary (it
/// replicates to the standbys). Expects `PGDATA`, `NODE_PORT`,
/// `AUTOCTL_NODE_PASSWORD` and `REPLICATION_PASSWORD`. Starts a background
/// retry loop.
///
/// During the in-place upgrade (`TEMPS_AUTH_UPGRADE=1`, see
/// [`PostgresClusterService::auth_upgrade_command`]), a standby that is still
/// taking its initial base backup has pg_autoctl configuration but no
/// database (`$PGDATA/PG_VERSION`) yet. It is not the primary and receives
/// every role from the primary with that backup, so it has nothing to set and
/// the loop exits 0 instead of waiting for a server that cannot start until
/// the backup completes. The entrypoint does not set the flag: there the loop
/// must keep waiting for `pg_autoctl run` to start PostgreSQL.
const NODE_PREPARE_SNIPPET: &str = r#"if gosu postgres pg_autoctl config get --pgdata "$PGDATA" postgresql.pgdata >/dev/null 2>&1; then
  gosu postgres pg_autoctl config set --pgdata "$PGDATA" replication.password "$REPLICATION_PASSWORD" >/dev/null 2>&1 || exit 1
  gosu postgres pg_autoctl config set --pgdata "$PGDATA" postgresql.auth_method scram-sha-256 >/dev/null 2>&1 || true
  CURRENT_MONITOR=$(gosu postgres pg_autoctl config get --pgdata "$PGDATA" pg_autoctl.monitor 2>/dev/null || true)
  if [ -n "$CURRENT_MONITOR" ]; then
    WANTED_MONITOR=$(printf '%s' "$CURRENT_MONITOR" | sed -E "s#^(postgres(ql)?://autoctl_node)(:[^@]*)?@#\1:${AUTOCTL_NODE_PASSWORD}@#")
    if [ "$WANTED_MONITOR" != "$CURRENT_MONITOR" ]; then
      gosu postgres pg_autoctl config set --pgdata "$PGDATA" pg_autoctl.monitor "$WANTED_MONITOR" >/dev/null 2>&1 || exit 1
    fi
  fi
fi
(
  for _ in $(seq 1 300); do
    if [ "${TEMPS_AUTH_UPGRADE:-}" = 1 ] && [ ! -f "$PGDATA/PG_VERSION" ]; then
      echo "temps: $PGDATA holds no database yet (standby awaiting its initial base backup); the replication password is set on the primary" >&2
      exit 0
    fi
    IN_RECOVERY=$(gosu postgres psql -X -At -p "$NODE_PORT" -d postgres -c "SELECT pg_is_in_recovery()" 2>/dev/null || true)
    if [ "$IN_RECOVERY" = "t" ]; then
      exit 0
    fi
    if [ "$IN_RECOVERY" = "f" ] && gosu postgres psql -X -At -p "$NODE_PORT" -d postgres -c "SELECT 1 FROM pg_roles WHERE rolname = 'pgautofailover_replicator'" 2>/dev/null | grep -q 1; then
      if printf '%s\n' "ALTER ROLE pgautofailover_replicator PASSWORD :'pw';" \
        | gosu postgres psql -X -q -p "$NODE_PORT" -d postgres -v ON_ERROR_STOP=1 -v pw="$REPLICATION_PASSWORD" >/dev/null 2>&1; then
        exit 0
      fi
    fi
    sleep 1
  done
  echo "temps: PostgreSQL on port $NODE_PORT did not accept local connections within 300s; the replication password was not confirmed on this member" >&2
  exit 1
) &"#;

/// Data node, enforce: rewrite every `trust` rule for
/// `pgautofailover_replicator` (the legacy `0.0.0.0/0` rules and
/// pg_auto_failover's own per-peer rules on clusters created with
/// `--auth trust`) to `scram-sha-256`. pg_auto_failover's monitor
/// health-check rule (`pgautofailover_monitor`, scoped to the monitor's
/// address) is left as pg_auto_failover generates it. Expects `PGDATA`.
const NODE_ENFORCE_SNIPPET: &str = r#"HBA="$PGDATA/pg_hba.conf"
if [ -f "$HBA" ]; then
  sed -i -E '/^[[:space:]]*host(ssl)?[[:space:]].*pgautofailover_replicator/ s/[[:space:]]trust([[:space:]]|$)/ scram-sha-256\1/' "$HBA"
  # PostgreSQL uses the first matching HBA rule. Keep permanent SCRAM
  # guards ahead of peer rules a legacy keeper may regenerate later.
  GUARDED_HBA=$(mktemp "$PGDATA/.temps-hba.XXXXXX")
  for CIDR in 0.0.0.0/0 ::/0; do
    for DATABASE in all replication; do
      printf 'hostnossl %s pgautofailover_replicator %s reject # Temps infrastructure authentication\n' "$DATABASE" "$CIDR"
      printf 'hostssl %s pgautofailover_replicator %s scram-sha-256 # Temps infrastructure authentication\n' "$DATABASE" "$CIDR"
    done
  done > "$GUARDED_HBA"
  sed '/# Temps infrastructure authentication$/d' "$HBA" >> "$GUARDED_HBA"
  mv "$GUARDED_HBA" "$HBA"
  chown postgres:postgres "$HBA"
  gosu postgres pg_ctl reload -D "$PGDATA" >/dev/null 2>&1 || true
fi"#;

/// Data node: create/update the configured application superuser and its
/// database. Written through a quoted heredoc and run by psql with the
/// variables `app_user`, `app_password` and `app_db`; values are quoted by
/// psql (`:'var'`) and `format()` (`%I` / `%L`), never spliced as text, so
/// any user-chosen password is safe. `password_encryption` is pinned so
/// the stored password is always a SCRAM verifier.
const APP_USER_SQL: &str = r#"SET password_encryption = 'scram-sha-256';
SELECT format('CREATE ROLE %I LOGIN SUPERUSER', :'app_user')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'app_user')\gexec
SELECT format('ALTER ROLE %I WITH LOGIN SUPERUSER PASSWORD %L', :'app_user', :'app_password')\gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'app_db', :'app_user')
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'app_db')\gexec"#;

/// Data node, application auth: choose the method of the application
/// catch-all rules (`host`/`hostssl all all 0.0.0.0/0` and `::/0`).
///
/// A `scram-sha-256` rule refuses a role whose stored password is an MD5
/// hash, while an `md5` rule accepts both MD5 and SCRAM verifiers (it then
/// performs SCRAM). Without the plaintext a stored MD5 hash cannot be
/// rehashed, so the rules are only switched to `scram-sha-256` once no
/// login role stores an MD5 hash; otherwise they are kept (or put back) at
/// `md5` and the reason is logged. pg_auto_failover writes
/// `password_encryption = '<--auth method>'`, so roles on SCRAM-native
/// members are SCRAM; MD5 hashes only appear from restored dumps or
/// explicit `password_encryption = md5`.
///
/// Must run after [`NODE_ENFORCE_SNIPPET`]: both rewrite `pg_hba.conf`.
/// Expects `PGDATA` and `NODE_PORT`. Starts a background job that exits 0
/// once the rules are decided (keeping `md5` is a decision), and 1 when
/// PostgreSQL never answered, so nothing was decided: the member keeps its
/// current rules, and an upgrade waiting on the job must not record the
/// cluster as converged. The entrypoint does not wait on it. During the
/// upgrade a member with no database yet exits 0, for the reason given on
/// [`NODE_PREPARE_SNIPPET`]: its base backup brings the primary's rules.
const NODE_APP_AUTH_SNIPPET: &str = r#"(
  HBA="$PGDATA/pg_hba.conf"
  APP_RULE='^([[:space:]]*host(ssl)?[[:space:]]+all[[:space:]]+all[[:space:]]+(0\.0\.0\.0/0|::/0)[[:space:]]+)(md5|scram-sha-256)([[:space:]]|$)'
  for _ in $(seq 1 300); do
    if [ "${TEMPS_AUTH_UPGRADE:-}" = 1 ] && [ ! -f "$PGDATA/PG_VERSION" ]; then
      echo "temps: $PGDATA holds no database yet (standby awaiting its initial base backup); it takes the primary's pg_hba rules with that backup" >&2
      exit 0
    fi
    MD5_ROLES=$(gosu postgres psql -X -At -p "$NODE_PORT" -d postgres -c "SELECT count(*) FROM pg_authid WHERE rolcanlogin AND rolpassword LIKE 'md5%'" 2>/dev/null || true)
    case "$MD5_ROLES" in
      ''|*[!0-9]*) sleep 1; continue ;;
    esac
    if [ "$MD5_ROLES" = 0 ]; then
      APP_AUTH=scram-sha-256
    else
      APP_AUTH=md5
      echo "temps: keeping md5 for the application pg_hba rules on port $NODE_PORT: $MD5_ROLES login role(s) store an MD5 password hash, which scram-sha-256 would refuse; reset those passwords with password_encryption = scram-sha-256 and restart this member to switch" >&2
    fi
    if [ -f "$HBA" ]; then
      APP_HBA=$(mktemp "$PGDATA/.temps-hba-app.XXXXXX")
      sed -E "s#${APP_RULE}#\\1${APP_AUTH}\\5#" "$HBA" > "$APP_HBA"
      if ! cmp -s "$HBA" "$APP_HBA"; then
        cat "$APP_HBA" > "$HBA"
        gosu postgres pg_ctl reload -D "$PGDATA" >/dev/null 2>&1 || true
        echo "temps: application pg_hba rules on port $NODE_PORT now use $APP_AUTH" >&2
      fi
      rm -f "$APP_HBA"
    fi
    exit 0
  done
  echo "temps: application pg_hba rules left unchanged: PostgreSQL on port $NODE_PORT did not accept local connections" >&2
  exit 1
) &"#;

/// Removes pg_autoctl's pidfile left behind by an unclean stop.
///
/// pg_autoctl writes it under `/tmp/pg_autoctl/<PGDATA path>/pg_autoctl.pid`
/// (e.g. `/tmp/pg_autoctl/var/lib/postgresql/monitor/pg_autoctl.pid`), and
/// the container's `/tmp` survives a restart. The previous
/// `rm -f /tmp/pg_autoctl/*.pid /tmp/pg_autoctl/*/*.pid` only reached two
/// levels, so after a host reboot or Docker daemon restart the new PID 1 found
/// "an instance already running with PID 1" — itself — and the member
/// crash-looped forever. Runs before `pg_autoctl run`, when nothing can own it.
const STALE_PIDFILE_CLEANUP: &str =
    "find /tmp/pg_autoctl -type f -name '*.pid' -delete 2>/dev/null || true";

/// One phase of the in-place SCRAM upgrade of a running cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthUpgradeStep {
    /// Set the `autoctl_node` password on the monitor (rules unchanged).
    MonitorPrepare,
    /// On every data node: `.pgpass`, pg_autoctl config, monitor URI and
    /// (on the primary) the replicator password (rules unchanged).
    NodePrepare,
    /// On every data node: switch replicator rules to SCRAM, and the
    /// application catch-all rules too once no login role stores an MD5
    /// hash (see [`NODE_APP_AUTH_SNIPPET`]).
    NodeEnforce,
    /// On the monitor: switch `autoctl_node` rules to SCRAM.
    MonitorEnforce,
}

impl AuthUpgradeStep {
    /// The order the steps must run in across the whole cluster.
    pub const ORDER: [AuthUpgradeStep; 4] = [
        AuthUpgradeStep::MonitorPrepare,
        AuthUpgradeStep::NodePrepare,
        AuthUpgradeStep::NodeEnforce,
        AuthUpgradeStep::MonitorEnforce,
    ];

    /// Whether this step runs on the monitor (otherwise on every data node).
    pub fn targets_monitor(self) -> bool {
        matches!(
            self,
            AuthUpgradeStep::MonitorPrepare | AuthUpgradeStep::MonitorEnforce
        )
    }
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
    fn monitor_env(
        &self,
        monitor_hostname: &str,
        monitor_port: u16,
        auth: &ClusterAuthSecrets,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("MONITOR_HOSTNAME".to_string(), monitor_hostname.to_string());
        env.insert("MONITOR_PORT".to_string(), monitor_port.to_string());
        env.insert(
            "AUTOCTL_NODE_PASSWORD".to_string(),
            auth.autoctl_node_password.clone(),
        );
        env
    }

    /// Build environment variables for a data node container.
    ///
    /// `monitor_port` is the port the monitor listens on (the mapped host port
    /// when using bridge networking, or the container port with host networking).
    /// `node_port` is the port this node will listen on.
    // Member network coordinates and credential bundle are intentionally explicit.
    #[allow(clippy::too_many_arguments)]
    fn node_env(
        &self,
        config: &PostgresClusterConfig,
        monitor_hostname: &str,
        monitor_port: u16,
        node_hostname: &str,
        node_port: u16,
        node_name: &str,
        auth: &ClusterAuthSecrets,
    ) -> HashMap<String, String> {
        let mut env = auth.env();
        env.insert("NODE_HOSTNAME".to_string(), node_hostname.to_string());
        env.insert("NODE_PORT".to_string(), node_port.to_string());
        env.insert("NODE_NAME".to_string(), node_name.to_string());
        // The secret is alphanumeric (see `ClusterAuthSecrets`), so it needs
        // no percent-encoding inside the URI.
        env.insert(
            "MONITOR_URI".to_string(),
            format!(
                "postgresql://autoctl_node:{}@{}:{}/pg_auto_failover",
                auth.autoctl_node_password,
                uri_host(monitor_hostname),
                monitor_port
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

    /// Command that runs one [`AuthUpgradeStep`] inside an already-running
    /// member container (`docker exec`, as root).
    ///
    /// The container's own environment supplies `MONITOR_PORT` /
    /// `NODE_PORT`; the caller must pass [`ClusterAuthSecrets::env`] as the
    /// exec environment. The command waits for its retry loop, so it returns
    /// once the step has converged (or the loop gave up), and exits non-zero
    /// when the role password could not be set on a node that needs it.
    pub fn auth_upgrade_command(step: AuthUpgradeStep) -> Vec<String> {
        let (pgdata, body) = match step {
            AuthUpgradeStep::MonitorPrepare => {
                ("/var/lib/postgresql/monitor", MONITOR_PREPARE_SNIPPET)
            }
            AuthUpgradeStep::MonitorEnforce => {
                ("/var/lib/postgresql/monitor", MONITOR_ENFORCE_SNIPPET)
            }
            AuthUpgradeStep::NodePrepare => ("/var/lib/postgresql/pgdata", NODE_PREPARE_SNIPPET),
            AuthUpgradeStep::NodeEnforce => ("/var/lib/postgresql/pgdata", NODE_ENFORCE_SNIPPET),
        };
        let mut script = vec![
            "set -e".to_string(),
            format!("PGDATA={pgdata}"),
            // Lets the shared snippets tell this exec from the entrypoint.
            "TEMPS_AUTH_UPGRADE=1".to_string(),
        ];
        if matches!(
            step,
            AuthUpgradeStep::MonitorPrepare | AuthUpgradeStep::NodePrepare
        ) {
            // Preserve legacy entrypoints' initialization guard on restart,
            // while asking pg_autoctl for its actual XDG config location.
            // A standby still taking its initial base backup has the config
            // but no `$PGDATA` directory yet; creating one would collide with
            // the backup that pg_autoctl moves into place, so it is skipped.
            script.push(
                r#"CONFIG_FILE=$(gosu postgres pg_autoctl show file --pgdata "$PGDATA" --config)
[ -f "$CONFIG_FILE" ]
if [ -d "$PGDATA" ] && [ "$CONFIG_FILE" != "$PGDATA/pg_autoctl.cfg" ] && [ ! -e "$PGDATA/pg_autoctl.cfg" ]; then
  ln -s "$CONFIG_FILE" "$PGDATA/pg_autoctl.cfg"
  chown -h postgres:postgres "$PGDATA/pg_autoctl.cfg"
fi"#
                .to_string(),
            );
        }
        if step == AuthUpgradeStep::NodePrepare {
            script.push(NODE_PGPASS_SNIPPET.to_string());
        }
        script.push(body.to_string());
        if step == AuthUpgradeStep::NodeEnforce {
            // Runs after the infrastructure rewrite (never concurrently with
            // it) and is waited for so the step returns converged. Keeping
            // `md5` is a logged decision and exits 0; a member whose
            // PostgreSQL never answered exits 1 so the step fails and the
            // cluster is not marked converged (it is retried next start).
            script.push(NODE_APP_AUTH_SNIPPET.to_string());
            script.push("wait $!".to_string());
        }
        if matches!(
            step,
            AuthUpgradeStep::MonitorPrepare | AuthUpgradeStep::NodePrepare
        ) {
            // Propagate the retry loop's outcome (it exits 1 on timeout).
            script.push("wait $!".to_string());
            // Existing keepers must load the new monitor/replication secrets
            // before any peer switches its HBA from trust to SCRAM.
            script.push(
                "gosu postgres pg_autoctl reload --pgdata \"$PGDATA\" >/dev/null 2>&1".to_string(),
            );
        }
        vec!["bash".to_string(), "-c".to_string(), script.join("\n")]
    }

    /// Build the startup command for the monitor container.
    ///
    /// The hostname is passed via the `MONITOR_HOSTNAME` environment variable
    /// so that it can be set to the worker node's WireGuard/private address
    /// when the monitor runs on a remote node.
    fn monitor_command(&self) -> Vec<String> {
        // The entrypoint script handles:
        // 1. pg_autoctl create monitor (if not initialized), with SCRAM auth
        // 2. Install SCRAM HBA + the role password on first creation, or
        //    resume an interrupted creation marked on the persistent volume.
        //    Existing volumes retain their authentication on restart: legacy
        //    peers must receive credentials through the staged cluster-wide
        //    upgrade before the monitor can stop accepting trust connections.
        // 3. Remove stale pidfile (prevents "already running with PID 1" on restart)
        // 4. pg_autoctl run
        //
        // Runs as the `postgres` user because pg_ctl refuses to run as root.
        vec![
            "bash".to_string(),
            "-c".to_string(),
            [
                "set -e",
                "PGDATA=/var/lib/postgresql/monitor",
                r#"AUTH_PENDING="${PGDATA%/*}/.temps-monitor-scram-initializing""#,
                "chown -R postgres:postgres /var/lib/postgresql",
                "if ! gosu postgres pg_autoctl config get --pgdata \"$PGDATA\" postgresql.pgdata >/dev/null 2>&1; then",
                "  touch \"$AUTH_PENDING\"",
                "  gosu postgres pg_autoctl create monitor \\",
                "    --pgdata \"$PGDATA\" \\",
                "    --pgport \"$MONITOR_PORT\" \\",
                "    --hostname \"$MONITOR_HOSTNAME\" \\",
                "    --auth scram-sha-256 \\",
                "    --ssl-self-signed;",
                "fi",
                "if [ -f \"$AUTH_PENDING\" ]; then",
                MONITOR_ENFORCE_SNIPPET,
                MONITOR_PREPARE_SNIPPET,
                "fi",
                STALE_PIDFILE_CLEANUP,
                "exec gosu postgres pg_autoctl run --pgdata \"$PGDATA\"",
            ]
            .join("\n"),
        ]
    }

    /// Build the startup command for a data node container.
    fn node_command(&self) -> Vec<String> {
        // The entrypoint script handles:
        // 1. Launch a background HBA patcher that waits for pg_hba.conf to appear
        //    and immediately adds SCRAM entries for replication connections.
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
                // as soon as it exists. pg_auto_failover only generates
                // replication rules for peer addresses it resolves itself,
                // which misses members reached over WireGuard/private
                // addresses, so pgautofailover_replicator gets explicit
                // rules — SCRAM-authenticated (password in ~/.pgpass, see
                // NODE_PGPASS_SNIPPET), never `trust`. This MUST run
                // concurrently with pg_autoctl create because the FSM
                // transition (primary → catchingup) happens inside `create`
                // before the command returns — sequential patching is too late.
                "(",
                "  while true; do",
                "    HBA=\"$PGDATA/pg_hba.conf\"",
                "    if [ -f \"$HBA\" ]; then",
                "      if ! grep -q 'pgautofailover_replicator.*0\\.0\\.0\\.0/0' \"$HBA\" 2>/dev/null; then",
                "        echo 'hostssl replication pgautofailover_replicator 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'hostssl replication pgautofailover_replicator ::/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host replication pgautofailover_replicator 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host replication pgautofailover_replicator ::/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'hostssl all pgautofailover_replicator 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'hostssl all pgautofailover_replicator ::/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host all pgautofailover_replicator 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host all pgautofailover_replicator ::/0 scram-sha-256' >> \"$HBA\"",
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
                // We add ONE catch-all SCRAM rule rather than a per-user
                // entry so future roles work without a code change.
                // Auth is still password-protected; the rule just
                // says "if the role exists and the password matches,
                // let it in from anywhere on the network the cluster
                // already trusts". pg_autoctl's `--auth scram-sha-256`
                // sets `password_encryption = 'scram-sha-256'`, and the
                // app user below is explicitly hashed with SCRAM.
                // NODE_APP_AUTH_SNIPPET puts the rules back to md5 if a
                // login role stores an MD5 hash (e.g. a standby cloned
                // from a legacy primary, or a restored dump).
                //
                // Non-TLS `host` rules stay: linked apps receive a
                // POSTGRES_URL without `sslmode`, and drivers whose
                // default is no TLS (node-postgres, JDBC) would be
                // refused by `hostnossl ... reject`.
                //
                // Order matters in pg_hba — the replicator rules above
                // this block and pg_auto_failover's auto-generated
                // monitor health-check rule match first.
                "      if ! grep -Eq '^host all all 0\\.0\\.0\\.0/0 (md5|scram-sha-256)' \"$HBA\" 2>/dev/null; then",
                "        echo 'hostssl all all 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'hostssl all all ::/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host all all 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "        echo 'host all all ::/0 scram-sha-256' >> \"$HBA\"",
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
                // Why a separate loop: pg_auto_failover's initdb leaves local
                // connections on `trust`, so the superuser has no
                // password, so external md5 auth always fails until we
                // ALTER it. We can't run this synchronously at script
                // top because Postgres isn't listening yet; we can't
                // batch it with the HBA patcher (which exits on first
                // patch) because Postgres might come up *after* the HBA
                // patcher finishes. So this is its own loop that retries
                // every 2s until the ALTER succeeds, then exits.
                //
                // The SQL is a fixed, quoted heredoc (`<<'SQL_EOF'`): the
                // shell expands nothing in it. The user, password and
                // database travel as psql variables and are quoted by
                // psql (`:'var'`) and by `format()` (`%I` identifier,
                // `%L` literal), so a password containing `'`, `$`,
                // backticks or backslashes can neither break nor extend
                // the SQL. (Splicing `${POSTGRES_PASSWORD}` into
                // `PASSWORD '...'` let a `'` in a user-chosen password
                // inject SQL running as superuser.)
                //
                // `password_encryption` is pinned for the session so the
                // app user always stores a SCRAM verifier, which the
                // `scram-sha-256` catch-all rule above requires.
                //
                // A standby is read-only and receives the role from the
                // primary through replication, so it skips the ALTER.
                //
                // We chmod the file 644 so `gosu postgres psql` (which
                // drops to the postgres user) can read it — without
                // this it lives as 600 root:root, every retry hits
                // EACCES, the loop times out and the password never
                // gets ALTERed, breaking auth for every external
                // caller including Browse Data.
                "(",
                "  SQL_FILE=$(mktemp /tmp/temps-app-user-XXXX.sql)",
                "  cat > \"$SQL_FILE\" <<'SQL_EOF'",
                APP_USER_SQL,
                "SQL_EOF",
                "  chmod 644 \"$SQL_FILE\"",
                "  for _ in $(seq 1 60); do",
                "    IN_RECOVERY=$(gosu postgres psql -X -At -p \"$NODE_PORT\" -d postgres -c 'SELECT pg_is_in_recovery()' 2>/dev/null || true)",
                "    if [ \"$IN_RECOVERY\" = t ]; then",
                "      break",
                "    fi",
                "    if [ \"$IN_RECOVERY\" = f ] && gosu postgres psql -X -q -p \"$NODE_PORT\" -d postgres -v ON_ERROR_STOP=1 \\",
                "      -v app_user=\"$POSTGRES_USER\" -v app_password=\"$POSTGRES_PASSWORD\" -v app_db=\"$POSTGRES_DB\" \\",
                "      -f \"$SQL_FILE\" >/dev/null 2>&1; then",
                "      break",
                "    fi",
                "    sleep 2",
                "  done",
                "  rm -f \"$SQL_FILE\"",
                ") &",
                NODE_PGPASS_SNIPPET,
                "if ! gosu postgres pg_autoctl config get --pgdata \"$PGDATA\" postgresql.pgdata >/dev/null 2>&1; then",
                "  gosu postgres pg_autoctl create postgres \\",
                "    --pgdata \"$PGDATA\" \\",
                "    --pgport \"$NODE_PORT\" \\",
                "    --hostname \"$NODE_HOSTNAME\" \\",
                "    --name \"$NODE_NAME\" \\",
                "    --auth scram-sha-256 \\",
                "    --ssl-self-signed \\",
                "    --monitor \"$MONITOR_URI\";",
                "fi",
                NODE_PREPARE_SNIPPET,
                NODE_ENFORCE_SNIPPET,
                NODE_APP_AUTH_SNIPPET,
                STALE_PIDFILE_CLEANUP,
                "exec gosu postgres pg_autoctl run --pgdata \"$PGDATA\"",
            ]
            .join("\n"),
        ]
    }
}

/// Docker-free, static metadata about this engine.
///
/// The parameter schema is generated from the input-config type and
/// depends on nothing at runtime, so it must be reachable without
/// constructing a service instance — a control plane with no local
/// Docker daemon still has to serve it to the console.
impl PostgresClusterService {
    /// JSON Schema describing this engine's creation parameters.
    pub fn parameter_schema() -> Option<serde_json::Value> {
        let schema = schemars::schema_for!(PostgresClusterConfig);
        serde_json::to_value(schema).ok()
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
        Self::parameter_schema()
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
                .map(|n| format!("{}:{}", uri_host(&n.hostname), n.port))
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
    /// Per-member cgroup limits. Defaults to unlimited; populated by the
    /// manager from the cluster's `ServiceConfig::parameters`.`resources`
    /// block so every member of a HA cluster ends up with the same caps.
    pub resource_limits: super::ServiceResourceLimits,
}

impl PostgresClusterService {
    /// Build creation parameters for a specific cluster member.
    ///
    /// * `monitor_hostname` — address the monitor advertises (host IP or container name)
    /// * `monitor_port` — port the monitor listens on (the host-mapped port)
    /// * `member_port` — port this member will listen on inside its container
    /// * `resource_limits` — cgroup limits applied to every member (monitor + data nodes)
    /// * `auth` — SCRAM secrets for the infrastructure roles (see [`ClusterAuthSecrets`])
    ///
    /// The manager uses these to create containers locally or via the agent.
    #[allow(clippy::too_many_arguments)]
    pub fn build_member_params(
        &self,
        member: &ClusterMemberSpec,
        config: &PostgresClusterConfig,
        monitor_hostname: &str,
        monitor_port: u16,
        member_port: u16,
        resource_limits: super::ServiceResourceLimits,
        auth: &ClusterAuthSecrets,
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
                environment: self.monitor_env(monitor_hostname, member_port, auth),
                command: Some(self.monitor_command()),
                container_port: member_port,
                volume_path: "/var/lib/postgresql".to_string(),
                resource_limits,
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
                        auth,
                    ),
                    command: Some(self.node_command()),
                    container_port: member_port,
                    volume_path: "/var/lib/postgresql".to_string(),
                    resource_limits,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_auth() -> ClusterAuthSecrets {
        ClusterAuthSecrets {
            autoctl_node_password: "MonitorSecret0123456789abcdefABCD".to_string(),
            replication_password: "ReplicaSecret0123456789abcdefABCD".to_string(),
        }
    }

    /// No pg_hba rule may admit pg_auto_failover's infrastructure roles with
    /// `trust`, and pg_autoctl must never be initialised with `--auth trust`.
    fn assert_no_trust_for_infrastructure_roles(script: &str) {
        assert!(!script.contains("--auth trust"), "{script}");
        for line in script.lines() {
            let is_rule = line.contains("echo '") && line.contains(">> \"$HBA\"");
            if is_rule
                && (line.contains("autoctl_node") || line.contains("pgautofailover_replicator"))
            {
                assert!(
                    !line.contains(" trust"),
                    "infrastructure role admitted with trust: {line}"
                );
            }
        }
    }

    #[test]
    fn generated_cluster_secrets_are_long_alphanumeric_and_distinct() {
        let a = ClusterAuthSecrets::generate();
        let b = ClusterAuthSecrets::generate();
        for secret in [&a.autoctl_node_password, &a.replication_password] {
            assert_eq!(secret.len(), CLUSTER_SECRET_LEN);
            assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
        }
        assert_ne!(a.autoctl_node_password, a.replication_password);
        assert_ne!(a, b);
    }

    #[test]
    fn cluster_secrets_round_trip_through_parameters() {
        let auth = test_auth();
        let mut params = HashMap::new();
        assert_eq!(ClusterAuthSecrets::from_parameters(&params).unwrap(), None);
        auth.insert_into(&mut params);
        assert_eq!(
            ClusterAuthSecrets::from_parameters(&params).unwrap(),
            Some(auth.clone())
        );
        // A partial credential pair must not rotate an existing password.
        params.remove(REPLICATION_PASSWORD_PARAM);
        assert!(matches!(
            ClusterAuthSecrets::from_parameters(&params),
            Err(ClusterAuthError::IncompleteCredentials)
        ));
    }

    #[test]
    fn cluster_secrets_reject_values_unsafe_for_uri_or_shell() {
        for bad in [
            serde_json::json!(""),
            serde_json::json!("has space"),
            serde_json::json!("quote'd"),
            serde_json::json!("at@sign"),
            serde_json::json!(42),
        ] {
            let mut params = HashMap::new();
            test_auth().insert_into(&mut params);
            params.insert(AUTOCTL_NODE_PASSWORD_PARAM.to_string(), bad.clone());
            assert!(
                ClusterAuthSecrets::from_parameters(&params).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn cluster_secret_parameter_names_are_masked_in_api_responses() {
        // `ExternalServiceManager::is_sensitive_parameter` masks every key
        // ending in `_password`; keep the names inside that convention.
        assert!(AUTOCTL_NODE_PASSWORD_PARAM.ends_with("_password"));
        assert!(REPLICATION_PASSWORD_PARAM.ends_with("_password"));
    }

    #[test]
    fn monitor_connection_string_carries_password_only_when_provisioned() {
        let legacy = monitor_connection_string("127.0.0.1", 6100, None);
        assert!(legacy.contains("user=autoctl_node"));
        assert!(legacy.contains("sslmode=require"));
        assert!(!legacy.contains("password="));

        let auth = test_auth();
        let scram = monitor_connection_string("127.0.0.1", 6100, Some(&auth));
        assert!(scram.contains("sslmode=require"));
        assert!(scram.ends_with(&format!("password={}", auth.autoctl_node_password)));
    }

    #[test]
    fn auth_upgrade_steps_distribute_passwords_before_enforcing() {
        let order = AuthUpgradeStep::ORDER;
        let position = |step| order.iter().position(|s| *s == step).unwrap();
        assert!(position(AuthUpgradeStep::MonitorPrepare) < position(AuthUpgradeStep::NodeEnforce));
        assert!(position(AuthUpgradeStep::NodePrepare) < position(AuthUpgradeStep::NodeEnforce));
        assert!(position(AuthUpgradeStep::NodePrepare) < position(AuthUpgradeStep::MonitorEnforce));
        assert!(position(AuthUpgradeStep::NodeEnforce) < position(AuthUpgradeStep::MonitorEnforce));

        let script = |step| PostgresClusterService::auth_upgrade_command(step)[2].clone();
        // Prepare steps never touch pg_hba; enforce steps only touch pg_hba.
        assert!(!script(AuthUpgradeStep::MonitorPrepare).contains("pg_hba"));
        assert!(!script(AuthUpgradeStep::NodePrepare).contains("pg_hba"));
        assert!(script(AuthUpgradeStep::NodeEnforce).contains("pg_hba"));
        assert!(script(AuthUpgradeStep::MonitorEnforce).contains("pg_hba"));
        // Prepare steps wait for their retry loop so failures surface.
        assert!(script(AuthUpgradeStep::NodePrepare).contains("wait $!"));
        assert!(script(AuthUpgradeStep::MonitorPrepare).contains("wait $!"));
        assert!(script(AuthUpgradeStep::NodePrepare).contains(".pgpass"));
        assert!(
            script(AuthUpgradeStep::MonitorEnforce).contains("PGDATA=/var/lib/postgresql/monitor")
        );
        assert!(script(AuthUpgradeStep::NodeEnforce).contains("PGDATA=/var/lib/postgresql/pgdata"));
        // Secrets come from the exec environment, never the command line.
        for step in order {
            let cmd = script(step);
            assert!(!cmd.contains(&test_auth().autoctl_node_password));
            assert!(!cmd.contains(&test_auth().replication_password));
        }
    }

    /// The legacy-rule rewrite must turn every `trust` rule for the
    /// infrastructure roles into SCRAM and leave every other rule alone.
    #[test]
    fn hba_rewrite_converts_only_infrastructure_trust_rules() {
        let sed_for = |snippet: &str| -> String {
            let line = snippet
                .lines()
                .find(|l| l.trim_start().starts_with("sed -i -E"))
                .expect("snippet rewrites pg_hba");
            line.trim().to_string()
        };
        let monitor_sed = sed_for(MONITOR_ENFORCE_SNIPPET);
        let node_sed = sed_for(NODE_ENFORCE_SNIPPET);
        let Ok(output) = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                r#"set -e
sed --version 2>/dev/null | grep -q GNU || exit 3
HBA=$(mktemp)
cat > "$HBA" <<'EOF'
local   all             all                                     trust
host    all             all             127.0.0.1/32            trust
hostssl "pg_auto_failover" "autoctl_node" 172.26.0.0/16 trust # Auto-generated by pg_auto_failover
hostssl pg_auto_failover autoctl_node 0.0.0.0/0 trust
hostssl replication pgautofailover_replicator 0.0.0.0/0 trust
host all pgautofailover_replicator ::/0 trust
hostssl "postgres" "pgautofailover_replicator" 172.26.0.4/32 trust # Auto-generated by pg_auto_failover
hostssl all "pgautofailover_monitor" 172.26.0.2/32 trust # Auto-generated by pg_auto_failover
hostssl all all 0.0.0.0/0 md5
EOF
{monitor_sed}
{node_sed}
cat "$HBA"
rm -f "$HBA""#
            ))
            .output()
        else {
            eprintln!("bash unavailable; skipping pg_hba rewrite test");
            return;
        };
        // `sed -i` differs between GNU and BSD; the snippets target the GNU
        // sed in the Debian member image. Skip where it is not available.
        if !output.status.success() {
            eprintln!(
                "GNU sed unavailable; skipping pg_hba rewrite test: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let hba = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = hba.lines().collect();
        assert_eq!(
            lines[0],
            "local   all             all                                     trust"
        );
        assert_eq!(
            lines[1],
            "host    all             all             127.0.0.1/32            trust"
        );
        assert_eq!(
            lines[2],
            "hostssl \"pg_auto_failover\" \"autoctl_node\" 172.26.0.0/16 scram-sha-256 # Auto-generated by pg_auto_failover"
        );
        assert_eq!(
            lines[3],
            "hostssl pg_auto_failover autoctl_node 0.0.0.0/0 scram-sha-256"
        );
        assert_eq!(
            lines[4],
            "hostssl replication pgautofailover_replicator 0.0.0.0/0 scram-sha-256"
        );
        assert_eq!(
            lines[5],
            "host all pgautofailover_replicator ::/0 scram-sha-256"
        );
        assert!(lines[6].contains("172.26.0.4/32 scram-sha-256 # Auto-generated"));
        // pg_auto_failover's own health-check rule is left as generated.
        assert!(lines[7].ends_with("172.26.0.2/32 trust # Auto-generated by pg_auto_failover"));
        assert_eq!(lines[8], "hostssl all all 0.0.0.0/0 md5");
    }

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

    #[cfg(unix)]
    #[test]
    fn restarting_an_initialized_monitor_preserves_existing_authentication() {
        use std::os::unix::fs::PermissionsExt;
        // Execute the actual entrypoint with isolated command stubs and a
        // temporary volume. No Docker daemon or operator resources are touched.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for command in ["chown", "rm"] {
            let path = bin.join(command);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let gosu = bin.join("gosu");
        std::fs::write(
            &gosu,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$COMMAND_LOG"
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(gosu, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The command generator never contacts Docker; use an HTTP client
        // constructor that does not require a local socket to exist.
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 120, bollard::API_DEFAULT_VERSION)
                .unwrap();
        let service = PostgresClusterService::new("test".into(), Arc::new(docker));
        let script = service.monitor_command()[2].replace(
            "/var/lib/postgresql/monitor",
            dir.path().join("monitor").to_str().unwrap(),
        );
        let pgdata = dir.path().join("monitor");
        std::fs::create_dir(&pgdata).unwrap();
        // Include a partially prepared legacy volume: setting auth_method
        // earlier in an interrupted upgrade does not prove peers are ready.
        for auth_method in ["trust", "scram-sha-256"] {
            let hba = format!("hostssl all autoctl_node 0.0.0.0/0 {auth_method}\n");
            std::fs::write(pgdata.join("pg_hba.conf"), &hba).unwrap();
            let log = dir.path().join("commands");
            std::fs::write(&log, "").unwrap();
            let output = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("COMMAND_LOG", &log)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "monitor restart failed: {output:?}"
            );
            assert_eq!(
                std::fs::read_to_string(pgdata.join("pg_hba.conf")).unwrap(),
                hba
            );
            let calls = std::fs::read_to_string(log).unwrap();
            assert!(calls.contains("pg_autoctl run"));
            assert!(!calls.contains("config set"));
            assert!(
                !calls.contains("psql"),
                "restart must not change credentials independently"
            );
        }
    }

    /// Regression for #1352: adding a replica re-ran the in-place upgrade on
    /// an existing standby still waiting for its initial base backup. It has
    /// pg_autoctl configuration but no `$PGDATA` directory, so NodePrepare
    /// failed with "ln: failed to create symbolic link .../pg_autoctl.cfg".
    /// Both data-node steps must instead converge without creating `$PGDATA`
    /// (pg_autoctl moves the backup into place there) and without waiting for
    /// a server that cannot start until the backup completes.
    #[test]
    fn data_node_upgrade_steps_converge_on_a_standby_without_a_database() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for command in ["chown", "chmod"] {
            let path = bin.join(command);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config_file = dir.path().join("xdg/pg_autoctl.cfg");
        std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        std::fs::write(&config_file, "[pg_autoctl]\n").unwrap();
        // pg_autoctl answers from its configuration; PostgreSQL is not
        // running, so every psql call fails.
        let gosu = bin.join("gosu");
        std::fs::write(
            &gosu,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$COMMAND_LOG"
case "$*" in
  *"pg_autoctl show file"*) printf '%s\n' "$CONFIG_FILE" ;;
  *psql*) exit 2 ;;
esac
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(gosu, std::fs::Permissions::from_mode(0o755)).unwrap();

        let pgdata = dir.path().join("pgdata");
        let pgpass = dir.path().join(".pgpass");
        for step in [AuthUpgradeStep::NodePrepare, AuthUpgradeStep::NodeEnforce] {
            let log = dir.path().join("commands");
            std::fs::write(&log, "").unwrap();
            let script = PostgresClusterService::auth_upgrade_command(step)[2]
                .replace("/var/lib/postgresql/pgdata", pgdata.to_str().unwrap())
                .replace("/var/lib/postgresql/.pgpass", pgpass.to_str().unwrap());
            let Ok(mut child) = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("COMMAND_LOG", &log)
                .env("CONFIG_FILE", &config_file)
                .env("NODE_PORT", "6201")
                .env("AUTOCTL_NODE_PASSWORD", "node-secret")
                .env("REPLICATION_PASSWORD", "replication-secret")
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                eprintln!("bash unavailable; skipping {step:?} script test");
                return;
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!(
                        "{step:?} waited for a server a standby without a database cannot start"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            let mut stderr = String::new();
            std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
            assert!(status.success(), "{step:?} failed: {stderr}");
            assert!(
                !pgdata.exists(),
                "{step:?} must not create PGDATA ahead of the base backup"
            );
            assert!(stderr.contains("no database yet"), "{step:?}: {stderr}");
            if step == AuthUpgradeStep::NodePrepare {
                // The standby still records the credentials it will use.
                let calls = std::fs::read_to_string(&log).unwrap();
                assert!(calls.contains("config set"), "{calls}");
                assert!(calls.contains("replication.password"), "{calls}");
                assert!(std::fs::read_to_string(&pgpass)
                    .unwrap()
                    .contains("pgautofailover_replicator:replication-secret"));
            }
        }
    }

    /// The no-database shortcut belongs to the exec'd upgrade only: in the
    /// entrypoint, the replicator-password loop must keep waiting for
    /// `pg_autoctl run` to start PostgreSQL on a freshly initialized primary.
    #[test]
    fn entrypoint_keeps_waiting_for_postgres_before_setting_the_replicator_password() {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 120, bollard::API_DEFAULT_VERSION)
                .unwrap();
        let service = PostgresClusterService::new("test".into(), Arc::new(docker));
        let entrypoint = &service.node_command()[2];
        assert!(!entrypoint.contains("TEMPS_AUTH_UPGRADE=1"));
        for step in [AuthUpgradeStep::NodePrepare, AuthUpgradeStep::NodeEnforce] {
            assert!(PostgresClusterService::auth_upgrade_command(step)[2]
                .contains("TEMPS_AUTH_UPGRADE=1"));
        }
    }

    /// Regression: after an unclean restart a member found its own stale
    /// pidfile ("already running with PID 1") and crash-looped, because the
    /// cleanup glob stopped two levels below where pg_autoctl writes it.
    #[test]
    fn stale_pidfile_cleanup_reaches_pg_autoctls_nested_pidfile() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("pg_autoctl");
        let nested = runtime.join("var/lib/postgresql/monitor");
        std::fs::create_dir_all(&nested).expect("create pidfile dir");
        let pidfile = nested.join("pg_autoctl.pid");
        let unrelated = nested.join("pg_autoctl.state");
        std::fs::write(&pidfile, "1\n").expect("write pidfile");
        std::fs::write(&unrelated, "keep").expect("write state");

        let script =
            STALE_PIDFILE_CLEANUP.replace("/tmp/pg_autoctl", &runtime.display().to_string());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .status()
            .expect("run cleanup");

        assert!(status.success());
        assert!(!pidfile.exists(), "the nested pidfile must be removed");
        assert!(unrelated.exists(), "only pidfiles are removed");

        // A first boot has no runtime dir at all; that must not fail the
        // `set -e` monitor script.
        let missing = STALE_PIDFILE_CLEANUP.replace(
            "/tmp/pg_autoctl",
            &root.path().join("absent").display().to_string(),
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -e\n{missing}"))
            .status()
            .expect("run cleanup");
        assert!(status.success());
    }

    #[test]
    fn member_commands_clear_stale_pidfiles_before_running() {
        let docker = Docker::connect_with_local_defaults().unwrap();
        let svc = PostgresClusterService::new("ha-pg".to_string(), Arc::new(docker));
        for script in [&svc.monitor_command()[2], &svc.node_command()[2]] {
            let cleanup = script
                .find(STALE_PIDFILE_CLEANUP)
                .expect("the start script must clear stale pidfiles");
            let run = script.find("pg_autoctl run").expect("runs pg_autoctl");
            assert!(
                cleanup < run,
                "cleanup must happen before pg_autoctl starts"
            );
            assert!(!script.contains("/tmp/pg_autoctl/*/*.pid"));
        }
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
        // autoctl_node (node registration) authenticates with SCRAM, never trust
        assert!(script.contains("--auth scram-sha-256"));
        assert!(script.contains("hostssl pg_auto_failover autoctl_node 0.0.0.0/0 scram-sha-256"));
        assert!(script.contains("ALTER ROLE autoctl_node PASSWORD :'pw'"));
        assert!(script.contains("$AUTOCTL_NODE_PASSWORD"));
        assert_no_trust_for_infrastructure_roles(script);
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
        assert!(script.contains("--auth scram-sha-256"));
        assert!(script.contains("replication pgautofailover_replicator 0.0.0.0/0 scram-sha-256"));
        // .pgpass is written before `create` clones a standby from the primary
        let pgpass = script.find(".pgpass").expect("pgpass written");
        let create = script
            .find("pg_autoctl create postgres")
            .expect("create runs");
        assert!(
            pgpass < create,
            "pgpass must exist before the standby clone"
        );
        assert!(script.contains("ALTER ROLE pgautofailover_replicator PASSWORD :'pw'"));
        assert_no_trust_for_infrastructure_roles(script);
    }

    /// Service whose command generators never contact Docker.
    fn offline_service() -> PostgresClusterService {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 120, bollard::API_DEFAULT_VERSION)
                .unwrap();
        PostgresClusterService::new("test".into(), Arc::new(docker))
    }

    /// The `pg_hba` lines a script appends for the application catch-all.
    fn app_catch_all_rules(script: &str) -> Vec<String> {
        script
            .lines()
            .filter(|l| l.contains("echo '") && l.contains(" all all "))
            .map(|l| l.trim().to_string())
            .collect()
    }

    #[test]
    fn node_command_app_catch_all_uses_scram_not_md5() {
        let script = offline_service().node_command()[2].clone();
        let rules = app_catch_all_rules(&script);
        assert_eq!(
            rules,
            [
                "echo 'hostssl all all 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "echo 'hostssl all all ::/0 scram-sha-256' >> \"$HBA\"",
                "echo 'host all all 0.0.0.0/0 scram-sha-256' >> \"$HBA\"",
                "echo 'host all all ::/0 scram-sha-256' >> \"$HBA\"",
            ]
        );
        // An existing catch-all of either method (a legacy md5 volume, or
        // a pg_hba.conf cloned from the primary) is not duplicated.
        assert!(script
            .contains("grep -Eq '^host all all 0\\.0\\.0\\.0/0 (md5|scram-sha-256)' \"$HBA\""));
        // Linked apps receive a POSTGRES_URL without `sslmode`, so non-TLS
        // application connections must keep working.
        assert!(!script.contains("hostnossl all all"), "{script}");
    }

    #[test]
    fn node_command_decides_app_auth_after_infrastructure_rewrite() {
        let script = offline_service().node_command()[2].clone();
        let enforce = script
            .find(NODE_ENFORCE_SNIPPET)
            .expect("infrastructure enforce runs");
        let app_auth = script
            .find(NODE_APP_AUTH_SNIPPET)
            .expect("application auth decision runs");
        assert!(
            enforce < app_auth,
            "both rewrite pg_hba.conf; they must not run concurrently"
        );
        assert!(app_auth < script.find("exec gosu postgres pg_autoctl run").unwrap());
        // The decision is based on stored hashes, never on assumptions.
        assert!(NODE_APP_AUTH_SNIPPET
            .contains("FROM pg_authid WHERE rolcanlogin AND rolpassword LIKE 'md5%'"));
    }

    #[test]
    fn auth_upgrade_node_enforce_applies_app_auth_decision_and_waits() {
        let node =
            PostgresClusterService::auth_upgrade_command(AuthUpgradeStep::NodeEnforce)[2].clone();
        let enforce = node.find(NODE_ENFORCE_SNIPPET).expect("enforce body");
        let app_auth = node.find(NODE_APP_AUTH_SNIPPET).expect("app auth decision");
        assert!(enforce < app_auth);
        assert!(node.trim_end().ends_with("wait $!"), "{node}");
        for step in [
            AuthUpgradeStep::MonitorPrepare,
            AuthUpgradeStep::NodePrepare,
            AuthUpgradeStep::MonitorEnforce,
        ] {
            let script = PostgresClusterService::auth_upgrade_command(step)[2].clone();
            assert!(
                !script.contains("rolpassword LIKE 'md5%'"),
                "{step:?} must not touch application auth"
            );
        }
    }

    /// Run [`NODE_APP_AUTH_SNIPPET`] against a temporary `pg_hba.conf`
    /// with `gosu` stubbed: `psql` reports `md5_roles`, `pg_ctl` is
    /// recorded. Returns `(pg_hba.conf, stderr, reloaded)`, or `None` when
    /// bash is unavailable.
    #[cfg(unix)]
    fn run_app_auth_snippet(hba: &str, md5_roles: &str) -> Option<(String, String, bool)> {
        let (hba, stderr, reloaded, succeeded) = run_app_auth_snippet_status(hba, md5_roles)?;
        assert!(succeeded, "snippet failed: {stderr}");
        Some((hba, stderr, reloaded))
    }

    /// Like [`run_app_auth_snippet`], also returning whether the job exited
    /// 0. `sleep` is stubbed so a PostgreSQL that never answers exhausts the
    /// retry loop immediately.
    #[cfg(unix)]
    fn run_app_auth_snippet_status(
        hba: &str,
        md5_roles: &str,
    ) -> Option<(String, String, bool, bool)> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gosu = bin.join("gosu");
        std::fs::write(
            &gosu,
            r#"#!/bin/sh
shift
case "$1" in
  psql) printf '%s\n' "$MD5_ROLES_STUB" ;;
  pg_ctl) printf 'reload\n' >> "$COMMAND_LOG" ;;
esac
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(gosu, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sleep = bin.join("sleep");
        std::fs::write(&sleep, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(sleep, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pgdata = dir.path().join("pgdata");
        std::fs::create_dir(&pgdata).unwrap();
        std::fs::write(pgdata.join("pg_hba.conf"), hba).unwrap();
        let log = dir.path().join("commands");
        std::fs::write(&log, "").unwrap();
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("{NODE_APP_AUTH_SNIPPET}\nwait $!"))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("PGDATA", &pgdata)
            .env("NODE_PORT", "5432")
            .env("MD5_ROLES_STUB", md5_roles)
            .env("COMMAND_LOG", &log)
            .output()
            .ok()?;
        let leftovers: Vec<_> = std::fs::read_dir(&pgdata)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "pg_hba.conf")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        Some((
            std::fs::read_to_string(pgdata.join("pg_hba.conf")).unwrap(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            std::fs::read_to_string(&log).unwrap().contains("reload"),
            output.status.success(),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn app_auth_fails_when_postgres_never_answers_so_the_upgrade_is_retried() {
        // `psql` prints nothing on every attempt, as when the server is down.
        let Some((hba, stderr, reloaded, succeeded)) =
            run_app_auth_snippet_status(LEGACY_NODE_HBA, "")
        else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert!(
            !succeeded,
            "an undecided member must fail the step, or the cluster is marked converged: {stderr}"
        );
        assert_eq!(hba, LEGACY_NODE_HBA, "rules must be left unchanged");
        assert!(!reloaded);
        assert!(
            stderr.contains("did not accept local connections") && stderr.contains("5432"),
            "{stderr}"
        );
    }

    const LEGACY_NODE_HBA: &str = "\
local   all             all                                     trust
hostnossl all pgautofailover_replicator 0.0.0.0/0 reject # Temps infrastructure authentication
hostssl all pgautofailover_replicator 0.0.0.0/0 scram-sha-256 # Temps infrastructure authentication
hostssl all all 0.0.0.0/0 md5
hostssl all all ::/0 md5
host all all 0.0.0.0/0 md5
host all all ::/0 md5
hostssl all \"pgautofailover_monitor\" 172.26.0.2/32 trust # Auto-generated by pg_auto_failover
host app_db app_user 10.0.0.0/8 md5
";

    #[cfg(unix)]
    #[test]
    fn app_auth_switches_catch_all_to_scram_when_no_md5_hashes_exist() {
        let Some((hba, stderr, reloaded)) = run_app_auth_snippet(LEGACY_NODE_HBA, "0") else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        let expected = LEGACY_NODE_HBA
            .replace("all all 0.0.0.0/0 md5", "all all 0.0.0.0/0 scram-sha-256")
            .replace("all all ::/0 md5", "all all ::/0 scram-sha-256");
        // Only the four catch-all lines change: initdb's local trust, the
        // infrastructure guards, pg_auto_failover's monitor rule and a
        // narrower operator rule are untouched.
        assert_eq!(hba, expected, "stderr: {stderr}");
        assert!(hba.contains("host app_db app_user 10.0.0.0/8 md5"));
        assert!(reloaded);
        assert!(stderr.contains("now use scram-sha-256"), "{stderr}");

        // Converged: a second run changes nothing and does not reload.
        let (again, _, reloaded) = run_app_auth_snippet(&expected, "0").unwrap();
        assert_eq!(again, expected);
        assert!(!reloaded);
    }

    #[cfg(unix)]
    #[test]
    fn app_auth_keeps_md5_and_explains_when_md5_hashes_exist() {
        let Some((hba, stderr, reloaded)) = run_app_auth_snippet(LEGACY_NODE_HBA, "2") else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert_eq!(hba, LEGACY_NODE_HBA);
        assert!(!reloaded);
        assert!(
            stderr.contains("keeping md5")
                && stderr.contains("2 login role(s) store an MD5 password hash"),
            "{stderr}"
        );

        // A SCRAM catch-all (new member cloned from a primary that still has
        // MD5-hashed roles) is put back to md5 so those roles can log in.
        let scram =
            LEGACY_NODE_HBA.replace("all all 0.0.0.0/0 md5", "all all 0.0.0.0/0 scram-sha-256");
        let (restored, _, reloaded) = run_app_auth_snippet(&scram, "1").unwrap();
        assert_eq!(restored, LEGACY_NODE_HBA);
        assert!(reloaded);
    }

    /// The app-user SQL heredoc is quoted: nothing in it is expanded by
    /// the shell, and the credentials only reach psql as variables.
    #[test]
    fn node_app_user_sql_never_splices_credentials() {
        let script = offline_service().node_command()[2].clone();
        assert!(script.contains("cat > \"$SQL_FILE\" <<'SQL_EOF'"));
        assert!(!script.contains("<<SQL_EOF"));
        for spliced in ["${POSTGRES_PASSWORD}", "${POSTGRES_USER}", "${POSTGRES_DB}"] {
            assert!(!script.contains(spliced), "{spliced} spliced into SQL");
        }
        assert!(script.contains(
            "-v app_user=\"$POSTGRES_USER\" -v app_password=\"$POSTGRES_PASSWORD\" -v app_db=\"$POSTGRES_DB\""
        ));
        // Values are quoted by psql and format(), never by hand.
        assert!(APP_USER_SQL.contains(
            "format('ALTER ROLE %I WITH LOGIN SUPERUSER PASSWORD %L', :'app_user', :'app_password')"
        ));
        assert!(
            APP_USER_SQL.contains("format('CREATE DATABASE %I OWNER %I', :'app_db', :'app_user')")
        );
        // The stored password is a SCRAM verifier whatever the server default.
        let pin = APP_USER_SQL
            .find("SET password_encryption = 'scram-sha-256';")
            .expect("password_encryption pinned");
        assert!(pin < APP_USER_SQL.find("PASSWORD %L").unwrap());
        // psql variables are not interpolated inside dollar-quoted bodies,
        // so the statement must not be wrapped in a DO block.
        assert!(!APP_USER_SQL.contains("$$"));
    }

    /// Run the generated heredoc with hostile credentials in the
    /// environment: the SQL file must be byte-for-byte [`APP_USER_SQL`].
    #[test]
    fn node_app_user_heredoc_is_not_expanded_by_the_shell() {
        let script = offline_service().node_command()[2].clone();
        let start = script
            .find("  cat > \"$SQL_FILE\" <<'SQL_EOF'")
            .expect("heredoc present");
        let end = script[start..]
            .find("\nSQL_EOF")
            .expect("heredoc terminated")
            + start;
        let heredoc = format!("{}\nSQL_EOF\ncat \"$SQL_FILE\"", &script[start..end]);
        let marker = std::env::temp_dir().join(format!("temps-heredoc-{}", uuid::Uuid::new_v4()));
        let Ok(output) = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "SQL_FILE=$(mktemp)\n{heredoc}\nrm -f \"$SQL_FILE\""
            ))
            .env("POSTGRES_USER", "app\"user")
            .env(
                "POSTGRES_PASSWORD",
                format!(
                    "pa'ss$(touch {m})`touch {m}`\\'; DROP ROLE x; --",
                    m = marker.display()
                ),
            )
            .env("POSTGRES_DB", "db'name")
            .output()
        else {
            eprintln!("bash unavailable; skipping");
            return;
        };
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            APP_USER_SQL
        );
        assert!(!marker.exists(), "password was executed by the shell");
    }

    #[test]
    fn standby_skips_app_user_alter() {
        let script = offline_service().node_command()[2].clone();
        let probe = script
            .find("-c 'SELECT pg_is_in_recovery()'")
            .expect("recovery probe");
        let alter = script.find("-f \"$SQL_FILE\"").expect("SQL applied");
        assert!(probe < alter);
        assert!(script.contains("if [ \"$IN_RECOVERY\" = f ] && gosu postgres psql"));
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

        let params = service.build_member_params(
            &spec,
            &config,
            "10.100.0.1",
            6100,
            6100,
            crate::externalsvc::ServiceResourceLimits::default(),
            &test_auth(),
        );
        assert_eq!(params.container_name, "postgres-my-db-monitor");
        assert_eq!(params.container_port, 6100);
        assert_eq!(params.image, DEFAULT_CLUSTER_IMAGE);
        // Monitor env should contain the hostname and port for pg_autoctl advertisement
        assert_eq!(
            params.environment.get("MONITOR_HOSTNAME").unwrap(),
            "10.100.0.1"
        );
        assert_eq!(params.environment.get("MONITOR_PORT").unwrap(), "6100");
        assert_eq!(
            params.environment.get("AUTOCTL_NODE_PASSWORD"),
            Some(&test_auth().autoctl_node_password)
        );
        assert!(
            !params.environment.contains_key("REPLICATION_PASSWORD"),
            "the monitor never needs the replication secret"
        );
    }

    #[test]
    fn uri_host_brackets_only_ipv6_literals() {
        assert_eq!(uri_host("fd00::10"), "[fd00::10]");
        assert_eq!(uri_host("::1"), "[::1]");
        assert_eq!(uri_host("10.52.0.10"), "10.52.0.10");
        assert_eq!(uri_host("postgres-ha-pg-monitor"), "postgres-ha-pg-monitor");
        assert_eq!(
            uri_host("ha-pg-1.ha-pg.temps.local"),
            "ha-pg-1.ha-pg.temps.local"
        );
        // Already bracketed is not a bare literal: left alone, not doubled.
        assert_eq!(uri_host("[fd00::10]"), "[fd00::10]");
    }

    /// Regression: a control plane whose private address is IPv6 put it
    /// bare into the data members' monitor URI
    /// (`...@fd00::10:6030/pg_auto_failover`), which libpq cannot parse, so
    /// worker data members could never register.
    #[test]
    fn ipv6_monitor_address_yields_a_parseable_monitor_uri() {
        use std::str::FromStr;

        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("ha-pg".to_string(), Arc::new(docker));
        let config: PostgresClusterConfig =
            serde_json::from_value(serde_json::json!({ "password": "secret" })).unwrap();
        let spec = ClusterMemberSpec {
            role: "replica".to_string(),
            node_id: Some(2),
            ordinal: 1,
            hostname: Some("fd00::21".to_string()),
        };

        let params = service.build_member_params(
            &spec,
            &config,
            "fd00::10",
            6030,
            6031,
            crate::externalsvc::ServiceResourceLimits::default(),
            &test_auth(),
        );

        let uri = params.environment.get("MONITOR_URI").unwrap();
        assert!(uri.contains("@[fd00::10]:6030/pg_auto_failover"), "{uri}");
        let parsed = tokio_postgres::Config::from_str(uri).expect("monitor URI must parse");
        assert_eq!(
            parsed.get_hosts(),
            &[tokio_postgres::config::Host::Tcp("fd00::10".to_string())]
        );
        assert_eq!(parsed.get_ports(), &[6030]);
        // pg_autoctl's `--hostname` takes the bare address.
        assert_eq!(params.environment.get("NODE_HOSTNAME").unwrap(), "fd00::21");
    }

    #[test]
    fn ipv6_members_yield_a_parseable_cluster_connection_string() {
        use std::str::FromStr;

        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("ha-pg".to_string(), Arc::new(docker));
        let member = |hostname: &str, port: i32| ClusterMemberInfo {
            role: "replica".to_string(),
            hostname: hostname.to_string(),
            port,
            status: "running".to_string(),
        };
        let config = ServiceConfig {
            name: "ha-pg".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({ "password": "secret" }),
        };

        let url = service
            .cluster_connection_string(
                &[member("fd00::21", 6031), member("10.52.0.22", 6032)],
                &config,
            )
            .unwrap();

        assert!(url.contains("@[fd00::21]:6031,10.52.0.22:6032/"), "{url}");
        let parsed = tokio_postgres::Config::from_str(&url).expect("cluster URL must parse");
        assert_eq!(
            parsed.get_hosts(),
            &[
                tokio_postgres::config::Host::Tcp("fd00::21".to_string()),
                tokio_postgres::config::Host::Tcp("10.52.0.22".to_string()),
            ]
        );
        assert_eq!(parsed.get_ports(), &[6031, 6032]);
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

        let params = service.build_member_params(
            &spec,
            &config,
            "10.100.0.1",
            6100,
            6101,
            crate::externalsvc::ServiceResourceLimits::default(),
            &test_auth(),
        );
        assert_eq!(params.container_name, "postgres-my-db-1");
        assert_eq!(params.container_port, 6101);
        assert_eq!(
            params.environment.get("MONITOR_URI").unwrap(),
            &format!(
                "postgresql://autoctl_node:{}@10.100.0.1:6100/pg_auto_failover",
                test_auth().autoctl_node_password
            )
        );
        assert_eq!(
            params.environment.get("REPLICATION_PASSWORD"),
            Some(&test_auth().replication_password)
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

/// Docker-backed checks of the SCRAM entrypoints and of the in-place upgrade
/// of a cluster created with the legacy `trust` entrypoints. They run real
/// pg_auto_failover clusters from [`DEFAULT_CLUSTER_IMAGE`] on a uniquely
/// named network and clean up only what they create.
///
/// Run: `cargo test --lib -p temps-providers --features docker-tests -- scram_docker`
#[cfg(all(test, feature = "docker-tests"))]
mod scram_docker_tests {
    use super::*;
    use bollard::exec::{CreateExecOptions, StartExecResults};
    use bollard::models::{
        ContainerCreateBody, HostConfig, NetworkCreateRequest, RestartPolicy, RestartPolicyNameEnum,
    };
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, LogsOptions, RemoveContainerOptions, RenameContainerOptions,
        StartContainerOptions, StopContainerOptions,
    };
    use futures::{FutureExt, StreamExt};
    use std::time::{Duration, Instant};

    const MONITOR_PGDATA: &str = "/var/lib/postgresql/monitor";

    struct Fixture {
        docker: Arc<Docker>,
        network: String,
        containers: Vec<String>,
        /// Create containers with the `unless-stopped` policy real cluster
        /// members run with, so a keeper that exits while its monitor is
        /// down comes back as it would in production. Off by default:
        /// tests that assert a deliberate exit need the container to stay
        /// down.
        restart_unless_stopped: bool,
    }

    impl Fixture {
        async fn new() -> Option<Self> {
            let docker = match Docker::connect_with_local_defaults() {
                Ok(d) => Arc::new(d),
                Err(e) => {
                    eprintln!("Docker not available, skipping: {e}");
                    return None;
                }
            };
            if docker.ping().await.is_err() {
                eprintln!("Docker daemon not responding, skipping");
                return None;
            }
            if docker.inspect_image(DEFAULT_CLUSTER_IMAGE).await.is_err() {
                use bollard::query_parameters::CreateImageOptionsBuilder;
                let mut pull = docker.create_image(
                    Some(
                        CreateImageOptionsBuilder::new()
                            .from_image(DEFAULT_CLUSTER_IMAGE)
                            .build(),
                    ),
                    None,
                    None,
                );
                tokio::time::timeout(Duration::from_secs(300), async {
                    while let Some(result) = pull.next().await {
                        result.expect("pull HA integration image");
                    }
                })
                .await
                .expect("HA image pull timed out");
            }
            let network = format!("temps-scram-it-{}", uuid::Uuid::new_v4().simple());
            docker
                .create_network(NetworkCreateRequest {
                    name: network.clone(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await
                .ok()?;
            Some(Self {
                docker,
                network,
                containers: Vec::new(),
                restart_unless_stopped: false,
            })
        }

        async fn run(&mut self, name: &str, env: &HashMap<String, String>, cmd: Vec<String>) {
            self.run_with_volume(name, env, cmd, None).await;
        }

        async fn run_with_volume(
            &mut self,
            name: &str,
            env: &HashMap<String, String>,
            cmd: Vec<String>,
            volume_from: Option<&str>,
        ) {
            let body = ContainerCreateBody {
                image: Some(DEFAULT_CLUSTER_IMAGE.to_string()),
                cmd: Some(cmd),
                env: Some(env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
                hostname: Some(name.to_string()),
                host_config: Some(HostConfig {
                    network_mode: Some(self.network.clone()),
                    volumes_from: volume_from.map(|name| vec![format!("{name}:rw")]),
                    restart_policy: self.restart_unless_stopped.then(|| RestartPolicy {
                        name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            self.docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(name).build()),
                    body,
                )
                .await
                .unwrap_or_else(|e| panic!("create {name}: {e}"));
            self.containers.push(name.to_string());
            self.docker
                .start_container(name, None::<StartContainerOptions>)
                .await
                .unwrap_or_else(|e| panic!("start {name}: {e}"));
        }

        async fn recreate_with_volume(
            &mut self,
            name: &str,
            env: &HashMap<String, String>,
            cmd: Vec<String>,
        ) {
            let retired = format!("{name}-retired");
            let state = self
                .docker
                .inspect_container(name, None)
                .await
                .unwrap()
                .state
                .unwrap();
            if state.running == Some(true) {
                self.docker
                    .stop_container(
                        name,
                        Some(StopContainerOptions {
                            t: Some(10),
                            ..Default::default()
                        }),
                    )
                    .await
                    .unwrap();
            }
            self.docker
                .rename_container(
                    name,
                    RenameContainerOptions {
                        name: retired.clone(),
                    },
                )
                .await
                .unwrap();
            *self
                .containers
                .iter_mut()
                .find(|entry| entry.as_str() == name)
                .unwrap() = retired.clone();
            self.run_with_volume(name, env, cmd, Some(&retired)).await;
        }

        async fn exec(
            &self,
            container: &str,
            cmd: Vec<String>,
            env: &HashMap<String, String>,
        ) -> (i64, String) {
            match self.try_exec(container, cmd, env).await {
                Ok(result) => result,
                Err(e) => panic!(
                    "create exec in {container}: {e}\n{}",
                    self.describe(container).await
                ),
            }
        }

        /// Like `exec`, but returns the error when the exec cannot be
        /// created, e.g. while the container is restarting.
        async fn try_exec(
            &self,
            container: &str,
            cmd: Vec<String>,
            env: &HashMap<String, String>,
        ) -> Result<(i64, String), bollard::errors::Error> {
            let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let exec = self
                .docker
                .create_exec(
                    container,
                    CreateExecOptions {
                        cmd: Some(cmd),
                        env: Some(env),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
            let mut out = String::new();
            if let Ok(StartExecResults::Attached { mut output, .. }) =
                self.docker.start_exec(&exec.id, None).await
            {
                while let Some(Ok(chunk)) = output.next().await {
                    out.push_str(&chunk.to_string());
                }
            }
            let code = self
                .docker
                .inspect_exec(&exec.id)
                .await
                .ok()
                .and_then(|i| i.exit_code)
                .unwrap_or(-1);
            Ok((code, out))
        }

        /// State, restart count and recent output of `container`, for
        /// failure messages: a container that is not running says why.
        async fn describe(&self, container: &str) -> String {
            let state = match self.docker.inspect_container(container, None).await {
                Ok(info) => {
                    let state = info.state.unwrap_or_default();
                    format!(
                        "{container}: status={:?} exit_code={:?} error={:?} restart_count={:?}",
                        state.status, state.exit_code, state.error, info.restart_count
                    )
                }
                Err(e) => format!("{container}: inspect failed: {e}"),
            };
            let mut logs = self.docker.logs(
                container,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "40".to_string(),
                    ..Default::default()
                }),
            );
            let mut tail = String::new();
            while let Some(Ok(chunk)) = logs.next().await {
                tail.push_str(&chunk.to_string());
            }
            format!("{state}\nlast log lines of {container}:\n{tail}")
        }

        /// Wait until `container` runs and its PostgreSQL accepts
        /// connections. After a restart the monitor still reports the
        /// member's last state, so `wait_for` alone can pass before the
        /// member is back.
        async fn wait_serving(&self, container: &str, timeout: Duration) {
            let start = Instant::now();
            loop {
                let probe = self
                    .try_exec(
                        container,
                        vec![
                            "pg_isready".into(),
                            "-h".into(),
                            "127.0.0.1".into(),
                            "-p".into(),
                            "5432".into(),
                        ],
                        &HashMap::new(),
                    )
                    .await;
                if matches!(probe, Ok((0, _))) {
                    return;
                }
                if start.elapsed() >= timeout {
                    panic!(
                        "{container} did not accept connections within {timeout:?} (last probe: {probe:?})\n{}",
                        self.describe(container).await
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }

        async fn sh(&self, container: &str, script: &str) -> (i64, String) {
            self.exec(
                container,
                vec!["bash".into(), "-c".into(), script.into()],
                &HashMap::new(),
            )
            .await
        }

        async fn state(&self, monitor: &str) -> String {
            self.sh(
                monitor,
                &format!("gosu postgres pg_autoctl show state --pgdata {MONITOR_PGDATA}"),
            )
            .await
            .1
        }

        /// Wait until `node` is reported (and assigned) `state`.
        async fn wait_for(&self, monitor: &str, node: &str, state: &str, timeout: Duration) {
            let start = Instant::now();
            loop {
                let current = self.state(monitor).await;
                let reached = current.lines().any(|l| {
                    let cols: Vec<&str> = l.split('|').map(str::trim).collect();
                    cols.first() == Some(&node)
                        && cols.len() >= 7
                        && cols[5] == state
                        && cols[6] == state
                });
                if reached {
                    return;
                }
                assert!(
                    start.elapsed() < timeout,
                    "{node} did not reach {state} within {timeout:?}:\n{current}"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }

        async fn cleanup(&self) {
            for name in &self.containers {
                let _ = self
                    .docker
                    .remove_container(
                        name,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            let _ = self.docker.remove_network(&self.network).await;
        }
    }

    fn psql_probe(host: &str, user: &str, db: &str, extra: &str) -> String {
        // PGPASSFILE=/dev/null: prove the server, not a local .pgpass,
        // decides. Exit status is psql's.
        format!(
            "PGPASSFILE=/dev/null PGCONNECT_TIMEOUT=5 psql -X -At \
             \"host={host} port=5432 user={user} dbname={db} sslmode=require {extra}\" \
             -c 'SELECT 1'"
        )
    }

    /// Every way a non-member could previously get in without credentials.
    async fn assert_infrastructure_roles_require_passwords(
        fx: &Fixture,
        from: &str,
        monitor: &str,
        data_nodes: &[&str],
        auth: &ClusterAuthSecrets,
    ) {
        let (code, out) = fx
            .sh(
                from,
                &psql_probe(monitor, "autoctl_node", "pg_auto_failover", ""),
            )
            .await;
        assert_ne!(
            code, 0,
            "autoctl_node without password must be refused: {out}"
        );
        let (code, out) = fx
            .sh(
                from,
                &psql_probe(
                    monitor,
                    "autoctl_node",
                    "pg_auto_failover",
                    &format!("password={}", auth.autoctl_node_password),
                ),
            )
            .await;
        assert_eq!(
            code, 0,
            "autoctl_node with its password must be accepted: {out}"
        );
        for node in data_nodes {
            let (code, out) = fx
                .sh(
                    from,
                    &psql_probe(node, "pgautofailover_replicator", "postgres", ""),
                )
                .await;
            assert_ne!(code, 0, "replicator on {node} without password: {out}");
            let (code, out) = fx
                .sh(
                    from,
                    &psql_probe(
                        node,
                        "pgautofailover_replicator",
                        "postgres",
                        "replication=true",
                    ),
                )
                .await;
            assert_ne!(code, 0, "replication on {node} without password: {out}");
            let (_, hba) = fx
                .sh(node, "grep -v '^#' /var/lib/postgresql/pgdata/pg_hba.conf")
                .await;
            assert_global_scram_guards_precede_peer_rules(
                &hba,
                "pgautofailover_replicator",
                &["all", "replication"],
            );
        }
        let (_, hba) = fx
            .sh(
                monitor,
                &format!("grep -v '^#' {MONITOR_PGDATA}/pg_hba.conf"),
            )
            .await;
        assert_global_scram_guards_precede_peer_rules(&hba, "autoctl_node", &["all"]);
    }

    fn assert_global_scram_guards_precede_peer_rules(hba: &str, role: &str, databases: &[&str]) {
        let lines: Vec<_> = hba.lines().filter(|line| line.contains(role)).collect();
        for (index, line) in lines.iter().enumerate() {
            if line.contains(" trust") {
                for database in databases {
                    for cidr in ["0.0.0.0/0", "::/0"] {
                        let guard = format!("hostssl {database} {role} {cidr} scram-sha-256");
                        assert!(
                            lines[..index].iter().any(|prior| prior.starts_with(&guard)),
                            "unprotected peer rule: {line}"
                        );
                    }
                }
            }
        }
        for database in databases {
            for cidr in ["0.0.0.0/0", "::/0"] {
                assert!(lines.iter().any(|line| line
                    .starts_with(&format!("hostssl {database} {role} {cidr} scram-sha-256"))));
                assert!(lines
                    .iter()
                    .any(|line| line
                        .starts_with(&format!("hostnossl {database} {role} {cidr} reject"))));
            }
        }
    }

    async fn controlled_failover(fx: &Fixture, monitor: &str) {
        let (code, out) = fx
            .sh(
                monitor,
                &format!(
                    "gosu postgres pg_autoctl perform failover --group 0 --pgdata {MONITOR_PGDATA}"
                ),
            )
            .await;
        assert_eq!(code, 0, "perform failover: {out}");
    }

    #[tokio::test]
    async fn scram_cluster_replicates_fails_over_and_refuses_passwordless_access() {
        let Some(mut fx) = Fixture::new().await else {
            return;
        };
        let suffix = &fx.network[fx.network.len() - 8..];
        let svc = PostgresClusterService::new(format!("scramit{suffix}"), fx.docker.clone());
        let auth = ClusterAuthSecrets::generate();
        let config = PostgresClusterConfig {
            database: "appdb".to_string(),
            username: "appuser".to_string(),
            password: Some("AppUserSecret123".to_string()),
            max_connections: 100,
            replicas: 1,
            docker_image: None,
            ssl_mode: "prefer".to_string(),
        };
        let monitor = svc.monitor_container_name();
        let members: Vec<ClusterMemberSpec> = (0..3)
            .map(|ordinal| ClusterMemberSpec {
                role: if ordinal == 0 { "monitor" } else { "replica" }.to_string(),
                node_id: None,
                ordinal,
                hostname: Some(if ordinal == 0 {
                    monitor.clone()
                } else {
                    svc.node_container_name(ordinal)
                }),
            })
            .collect();

        let result = std::panic::AssertUnwindSafe(async {
            for spec in &members {
                let params = svc.build_member_params(
                    spec,
                    &config,
                    &monitor,
                    5432,
                    5432,
                    crate::externalsvc::ServiceResourceLimits::default(),
                    &auth,
                );
                let command = params.command.clone().expect("member command");
                if spec.ordinal == 0 {
                    // Crash after pg_autoctl has persisted its configuration,
                    // before authentication initialization completes. The
                    // pending marker must survive on this test's volume.
                    let mut interrupted = command.clone();
                    interrupted[2] = interrupted[2].replace(
                        MONITOR_ENFORCE_SNIPPET,
                        "test -f \"$AUTH_PENDING\" || exit 76\nexit 75",
                    );
                    fx.run(&params.container_name, &params.environment, interrupted)
                        .await;
                    tokio::time::timeout(Duration::from_secs(120), async {
                        loop {
                            let state = fx
                                .docker
                                .inspect_container(&params.container_name, None)
                                .await
                                .unwrap()
                                .state
                                .unwrap();
                            if state.running != Some(true) {
                                assert_eq!(
                                    state.exit_code,
                                    Some(75),
                                    "must interrupt after config and marker persist"
                                );
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    })
                    .await
                    .expect("interrupted monitor must exit");
                    fx.recreate_with_volume(&params.container_name, &params.environment, command)
                        .await;
                } else {
                    fx.run(&params.container_name, &params.environment, command)
                        .await;
                }
                if spec.ordinal == 1 {
                    fx.wait_for(
                        &monitor,
                        &params.container_name,
                        "single",
                        Duration::from_secs(240),
                    )
                    .await;
                }
            }
            let n1 = svc.node_container_name(1);
            let n2 = svc.node_container_name(2);
            fx.wait_for(&monitor, &n1, "primary", Duration::from_secs(240))
                .await;
            fx.wait_for(&monitor, &n2, "secondary", Duration::from_secs(240))
                .await;

            let (code, out) = fx
                .sh(
                    &monitor,
                    "test ! -f /var/lib/postgresql/.temps-monitor-scram-initializing",
                )
                .await;
            assert_eq!(
                code, 0,
                "successful recovery must clear the pending marker: {out}"
            );

            assert_infrastructure_roles_require_passwords(&fx, &n2, &monitor, &[&n1, &n2], &auth)
                .await;

            controlled_failover(&fx, &monitor).await;
            fx.wait_for(&monitor, &n2, "primary", Duration::from_secs(180))
                .await;
            fx.wait_for(&monitor, &n1, "secondary", Duration::from_secs(180))
                .await;

            // A cluster marked converged at auth version 1 still has `md5`
            // application rules until each member restarts. The version-2
            // upgrade re-runs only `NodeEnforce`; on a running member with no
            // MD5 hash that alone must switch them, on primary and standby.
            let app_rules = "grep -E '^host(ssl)? all all (0\\.0\\.0\\.0/0|::/0) ' \
                             /var/lib/postgresql/pgdata/pg_hba.conf";
            for node in [&n1, &n2] {
                let (code, out) = fx
                    .sh(
                        node,
                        "sed -i -E 's#^(host(ssl)? all all (0\\.0\\.0\\.0/0|::/0) )scram-sha-256#\\1md5#' \
                         /var/lib/postgresql/pgdata/pg_hba.conf \
                         && gosu postgres pg_ctl reload -D /var/lib/postgresql/pgdata",
                    )
                    .await;
                assert_eq!(code, 0, "{node}: put app rules back to md5: {out}");
                let (_, before) = fx.sh(node, app_rules).await;
                assert!(
                    before.lines().count() == 4 && before.lines().all(|l| l.ends_with(" md5")),
                    "{node}: expected four md5 app rules before the upgrade: {before}"
                );

                let (code, out) = fx
                    .exec(
                        node,
                        PostgresClusterService::auth_upgrade_command(AuthUpgradeStep::NodeEnforce),
                        &auth.env(),
                    )
                    .await;
                assert_eq!(code, 0, "{node}: NodeEnforce: {out}");
                let (_, after) = fx.sh(node, app_rules).await;
                assert!(
                    after.lines().count() == 4
                        && after.lines().all(|l| l.ends_with(" scram-sha-256")),
                    "{node}: app rules must switch to scram-sha-256: {after}"
                );
            }
            // The application user still authenticates over TCP afterwards.
            let (code, out) = fx
                .sh(
                    &n2,
                    &format!(
                        "PGPASSWORD=AppUserSecret123 psql -X -At \
                         \"host={n2} port=5432 user=appuser dbname=appdb sslmode=require\" \
                         -c 'SELECT 1'"
                    ),
                )
                .await;
            assert!(code == 0 && out.contains('1'), "app user login after upgrade: {out}");
        })
        .catch_unwind()
        .await;
        fx.cleanup().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    async fn legacy_trust_cluster_upgrades_in_place_to_scram() {
        let Some(mut fx) = Fixture::new().await else {
            return;
        };
        // This test restarts members while their monitor restarts too.
        fx.restart_unless_stopped = true;
        let suffix = fx.network[fx.network.len() - 8..].to_string();
        let monitor = format!("legacy{suffix}-monitor");
        let n1 = format!("legacy{suffix}-1");
        let n2 = format!("legacy{suffix}-2");
        let auth = ClusterAuthSecrets::generate();

        let result = std::panic::AssertUnwindSafe(async {
            // A cluster exactly as the pre-SCRAM entrypoints built it.
            fx.run(
                &monitor,
                &HashMap::from([
                    ("MONITOR_HOSTNAME".to_string(), monitor.clone()),
                    ("MONITOR_PORT".to_string(), "5432".to_string()),
                ]),
                legacy_monitor_command(),
            )
            .await;
            for (node, wait_state) in [(&n1, "single"), (&n2, "secondary")] {
                fx.run(
                    node,
                    &HashMap::from([
                        ("NODE_HOSTNAME".to_string(), node.clone()),
                        ("NODE_PORT".to_string(), "5432".to_string()),
                        ("NODE_NAME".to_string(), node.clone()),
                        (
                            "MONITOR_URI".to_string(),
                            format!("postgresql://autoctl_node@{monitor}:5432/pg_auto_failover"),
                        ),
                        ("POSTGRES_USER".to_string(), "appuser".to_string()),
                        (
                            "POSTGRES_PASSWORD".to_string(),
                            "AppUserSecret123".to_string(),
                        ),
                        ("POSTGRES_DB".to_string(), "appdb".to_string()),
                    ]),
                    legacy_node_command(),
                )
                .await;
                fx.wait_for(&monitor, node, wait_state, Duration::from_secs(240))
                    .await;
            }
            fx.wait_for(&monitor, &n1, "primary", Duration::from_secs(120))
                .await;

            // Sanity: the legacy cluster really is open.
            let (code, _) = fx
                .sh(
                    &n2,
                    &psql_probe(&n1, "pgautofailover_replicator", "postgres", ""),
                )
                .await;
            assert_eq!(
                code, 0,
                "legacy cluster admits the replicator without a password"
            );

            // Recreate only this test's monitor with the current entrypoint
            // and the legacy volume, before any peer has received passwords.
            // Restarting one member must not perform a cluster-wide upgrade.
            let mut env = auth.env();
            env.insert("MONITOR_HOSTNAME".into(), monitor.clone());
            env.insert("MONITOR_PORT".into(), "5432".into());
            let svc = PostgresClusterService::new("test".into(), fx.docker.clone());
            fx.recreate_with_volume(&monitor, &env, svc.monitor_command()).await;
            fx.wait_for(&monitor, &n1, "primary", Duration::from_secs(120)).await;
            fx.wait_for(&monitor, &n2, "secondary", Duration::from_secs(120)).await;
            let (code, out) = fx.sh(&n1, &psql_probe(&monitor, "autoctl_node", "pg_auto_failover", "")).await;
            assert_eq!(code, 0, "legacy peers must still reach the recreated monitor without a password: {out}");

            // In-place upgrade, in the order the control plane runs it.
            for step in AuthUpgradeStep::ORDER {
                let targets: Vec<&String> = if step.targets_monitor() {
                    vec![&monitor]
                } else {
                    vec![&n1, &n2]
                };
                for target in targets {
                    let (code, out) = fx
                        .exec(
                            target,
                            PostgresClusterService::auth_upgrade_command(step),
                            &auth.env(),
                        )
                        .await;
                    assert_eq!(code, 0, "{step:?} on {target}: {out}");
                }
            }
            for node in [&n1, &n2] {
                let (code, uri) = fx.sh(node, "gosu postgres pg_autoctl config get --pgdata /var/lib/postgresql/pgdata pg_autoctl.monitor").await;
                assert_eq!(code, 0, "keeper configuration must be readable");
                assert!(uri.contains(&format!("autoctl_node:{}@", auth.autoctl_node_password)), "keeper monitor credentials were not installed");
            }
            // Re-running is a no-op.
            for step in AuthUpgradeStep::ORDER {
                let target = if step.targets_monitor() {
                    &monitor
                } else {
                    &n1
                };
                let (code, out) = fx
                    .exec(
                        target,
                        PostgresClusterService::auth_upgrade_command(step),
                        &auth.env(),
                    )
                    .await;
                assert_eq!(code, 0, "re-run {step:?} on {target}: {out}");
            }

            fx.wait_for(&monitor, &n1, "primary", Duration::from_secs(120))
                .await;
            fx.wait_for(&monitor, &n2, "secondary", Duration::from_secs(120))
                .await;
            assert_infrastructure_roles_require_passwords(&fx, &n2, &monitor, &[&n1, &n2], &auth)
                .await;

            // Keepers keep talking to the monitor with the new credentials:
            // a controlled failover round-trips through both of them.
            controlled_failover(&fx, &monitor).await;
            fx.wait_for(&monitor, &n2, "primary", Duration::from_secs(180))
                .await;
            fx.wait_for(&monitor, &n1, "secondary", Duration::from_secs(180))
                .await;

            // Restart the legacy node and the recreated monitor; neither
            // entrypoint may reopen trust or lose the member.
            fx.docker
                .restart_container(
                    &n1,
                    None::<bollard::query_parameters::RestartContainerOptions>,
                )
                .await
                .unwrap_or_else(|e| panic!("restart {n1}: {e}"));
            fx.docker
                .restart_container(
                    &monitor,
                    None::<bollard::query_parameters::RestartContainerOptions>,
                )
                .await
                .unwrap_or_else(|e| panic!("restart {monitor}: {e}"));
            fx.wait_serving(&monitor, Duration::from_secs(240)).await;
            fx.wait_serving(&n1, Duration::from_secs(240)).await;
            fx.wait_for(&monitor, &n1, "secondary", Duration::from_secs(240))
                .await;
            assert_infrastructure_roles_require_passwords(&fx, &n2, &monitor, &[&n1, &n2], &auth)
                .await;
        })
        .catch_unwind()
        .await;
        fx.cleanup().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    // ── Legacy (pre-SCRAM) entrypoints, verbatim, as upgrade fixtures ──────

    fn legacy_monitor_command() -> Vec<String> {
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

    fn legacy_node_command() -> Vec<String> {
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
