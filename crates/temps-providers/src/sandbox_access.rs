// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credentials a sandbox can use against a linked PostgreSQL service
//! (ADR-051).
//!
//! A deployment connects as the cluster's administrator to a database named
//! for its environment. A sandbox runs code nobody reviewed, so it gets its
//! own database (`_sandbox_<project>`, or `_sandboxid_<sandbox>` for a sandbox
//! linked directly; a namespace no deployment can use, see
//! [`crate::sandbox_engines::SANDBOX_DATABASE_PREFIX`]) and a role that owns
//! that database and can reach nothing else. A database the sandbox did not
//! create is never re-owned or handed to it: ownership by the sandbox's role is
//! the provenance, and only the administrator can set it. Names, the derived
//! password, the SQL and the decision are pure; [`apply`] runs them.

use std::collections::HashMap;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::externalsvc::postgres::PostgresService;
use crate::externalsvc::scoped_resource_name;
pub use temps_providers_core::sandbox_namespace::sandbox_role_name;
use temps_providers_core::sandbox_namespace::{IDENTIFIER_MAX, ROLE_PREFIX};

use crate::sandbox_engines::{
    choose_sandbox_resource, ExistingResource, SandboxAccessEngine, SandboxAccessRequest,
    SandboxNamespace, SandboxNamespaceConflict, SandboxResourceChoice,
};
use crate::services::{ExternalServiceError, ExternalServiceManager};
use temps_entities::external_services;
use tracing::info;

/// The environment slot sandboxes used in a project's database naming before
/// they had a namespace of their own (`<project>_sandbox`).
const LEGACY_SANDBOX_SCOPE: &str = "sandbox";
const ENGINE: &str = "PostgreSQL";
/// PostgreSQL truncates identifiers at 63 bytes.
/// Connections the role may hold at once, so a runaway loop in a sandbox
/// cannot exhaust the cluster's connection slots.
const ROLE_CONNECTION_LIMIT: u32 = 20;
/// Tag of the HMAC input. Bump it to rotate every derived password at once.
const PASSWORD_DERIVATION_TAG: &str = "temps-scoped-role-v1";

type HmacSha256 = Hmac<Sha256>;

/// Why a plan could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxPlanError {
    /// A name that would be spliced into SQL as an identifier is not made of
    /// `[a-z0-9_]`.
    UnsafeIdentifier { kind: &'static str, value: String },
    /// The administrator password could not key the HMAC.
    PasswordDerivation { reason: String },
}

impl std::fmt::Display for SandboxPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeIdentifier { kind, value } => write!(
                formatter,
                "the {kind} name '{value}' is not a plain lowercase identifier ([a-z0-9_], at \
                 most {IDENTIFIER_MAX} characters, not starting with a digit)"
            ),
            Self::PasswordDerivation { reason } => {
                write!(formatter, "could not derive the sandbox password: {reason}")
            }
        }
    }
}

impl std::error::Error for SandboxPlanError {}

/// The name a project's sandbox database had before sandboxes got a
/// namespace of their own: `<project>_sandbox`. It is exactly the name a
/// deployment gives an environment slugged `sandbox` (and, in project mode, a
/// project slugged `<project>-sandbox`), so a database under it is only used
/// when its owner proves it is a sandbox's; see [`apply`].
pub fn legacy_sandbox_database_name(project_slug: &str) -> String {
    PostgresService::normalize_database_name(&scoped_resource_name(
        project_slug,
        LEGACY_SANDBOX_SCOPE,
    ))
}

/// What the database `database` is to the sandbox role `role`, from its owner
/// (`None` when it does not exist). Only the administrator can make a role a
/// database's owner, and the sandbox role cannot give its database away
/// (`NOCREATEDB`), so ownership is a provenance the sandbox cannot forge.
pub fn classify_database(owner: Option<&str>, role: &str) -> ExistingResource {
    match owner {
        None => ExistingResource::Missing,
        Some(owner) if owner == role => ExistingResource::Sandbox,
        Some(owner) => ExistingResource::Foreign {
            evidence: format!(
                "it is owned by the role '{owner}', not by the sandbox role '{role}'"
            ),
        },
    }
}

/// A stable password for the role: it survives restarts without being stored,
/// and changes when the administrator password does.
pub fn derive_role_password(
    admin_password: &str,
    service_id: i32,
    database: &str,
) -> Result<String, SandboxPlanError> {
    let mut mac = HmacSha256::new_from_slice(admin_password.as_bytes()).map_err(|error| {
        SandboxPlanError::PasswordDerivation {
            reason: error.to_string(),
        }
    })?;
    mac.update(format!("{PASSWORD_DERIVATION_TAG}:{service_id}:{database}").as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn is_plain_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= IDENTIFIER_MAX
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.as_bytes()[0].is_ascii_digit()
}

/// SQL string literal for a value that is not an identifier (the administrator's
/// user name, which operators choose freely).
fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Everything needed to give a sandbox its database, as SQL.
///
/// Every statement is idempotent, so provisioning can run on each connect and
/// repairs a role or database that drifted.
#[derive(Clone, PartialEq, Eq)]
pub struct SandboxDatabasePlan {
    pub database: String,
    pub role: String,
    password: String,
    admin_user: String,
}

impl std::fmt::Debug for SandboxDatabasePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxDatabasePlan")
            .field("database", &self.database)
            .field("role", &self.role)
            .field("password", &"***")
            .finish()
    }
}

impl SandboxDatabasePlan {
    /// The plan for the sandbox namespace: see
    /// [`SandboxNamespace::database_name`].
    pub fn new(
        service_id: i32,
        namespace: &SandboxNamespace,
        admin_user: &str,
        admin_password: &str,
    ) -> Result<Self, SandboxPlanError> {
        Self::for_database(
            service_id,
            namespace.database_name(),
            admin_user,
            admin_password,
        )
    }

    /// The plan for the pre-namespace name `<project>_sandbox`, used only for
    /// a database the sandbox role already owns.
    pub fn legacy(
        service_id: i32,
        project_slug: &str,
        admin_user: &str,
        admin_password: &str,
    ) -> Result<Self, SandboxPlanError> {
        Self::for_database(
            service_id,
            legacy_sandbox_database_name(project_slug),
            admin_user,
            admin_password,
        )
    }

    fn for_database(
        service_id: i32,
        database: String,
        admin_user: &str,
        admin_password: &str,
    ) -> Result<Self, SandboxPlanError> {
        if !is_plain_identifier(&database) {
            return Err(SandboxPlanError::UnsafeIdentifier {
                kind: "database",
                value: database,
            });
        }
        let role = sandbox_role_name(&database);
        if !is_plain_identifier(&role) {
            return Err(SandboxPlanError::UnsafeIdentifier {
                kind: "role",
                value: role,
            });
        }
        let password = derive_role_password(admin_password, service_id, &database)?;
        Ok(Self {
            database,
            role,
            password,
            admin_user: admin_user.to_string(),
        })
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    /// Create the role, or put an existing one back to the intended
    /// attributes and password.
    pub fn role_statement(&self) -> String {
        let attributes = format!(
            "LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS \
             CONNECTION LIMIT {ROLE_CONNECTION_LIMIT} PASSWORD '{}'",
            self.password
        );
        format!(
            "DO $sbx$ BEGIN \
             IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {literal}) THEN \
             CREATE ROLE \"{role}\" {attributes}; \
             ELSE ALTER ROLE \"{role}\" {attributes}; \
             END IF; END $sbx$",
            literal = quote_literal(&self.role),
            role = self.role,
        )
    }

    /// The owner of a database, bound with its name as `$1`; no row when it
    /// does not exist. Read before anything is changed, so a database that is
    /// not the sandbox's is never touched.
    pub const DATABASE_OWNER_QUERY: &'static str =
        "SELECT pg_catalog.pg_get_userbyid(datdba)::text FROM pg_catalog.pg_database \
         WHERE datname = $1";

    /// `CREATE DATABASE` cannot run inside a function or transaction, so it is
    /// its own statement, chosen by the caller after the ownership check. The
    /// database is created owned by the role: that ownership is what proves,
    /// on every later connect, that the database is the sandbox's.
    ///
    /// There is deliberately no statement that re-owns an existing database:
    /// one the role does not own already was not created for this sandbox
    /// (it may be a deployment's) and is refused, never adopted.
    pub fn create_database_statement(&self) -> String {
        format!(
            "CREATE DATABASE \"{}\" OWNER \"{}\"",
            self.database, self.role
        )
    }

    /// The administrator's databases where `PUBLIC` still holds `CONNECT`:
    /// the ones [`Self::lockdown_statement`] is about to change. Shared by the
    /// query that reports them and the block that changes them, so the two
    /// cannot disagree.
    fn open_databases_condition(&self) -> String {
        format!(
            "datallowconn AND NOT datistemplate AND datname <> 'postgres' \
             AND datname <> {database_literal} \
             AND datdba = (SELECT oid FROM pg_roles WHERE rolname = {admin_literal}) \
             AND (datacl IS NULL OR EXISTS (SELECT 1 FROM aclexplode(datacl) a \
             WHERE a.grantee = 0 AND a.privilege_type = 'CONNECT'))",
            database_literal = quote_literal(&self.database),
            admin_literal = quote_literal(&self.admin_user),
        )
    }

    /// The names of the databases the lockdown will close to `PUBLIC`, read in
    /// the same round trip just before it runs so the change can be logged.
    pub fn lockdown_targets_query(&self) -> String {
        format!(
            "SELECT datname::text FROM pg_database WHERE {} ORDER BY datname",
            self.open_databases_condition()
        )
    }

    /// Keep the role out of every other database.
    ///
    /// New databases let anyone connect (`PUBLIC` holds `CONNECT`), so the role
    /// could open all of them. This takes that right away from `PUBLIC` on the
    /// sandbox database and on the databases the administrator owns (the
    /// per-environment ones Temps creates). It leaves `postgres`, the
    /// templates, and databases owned by other roles as they were, so tools
    /// that rely on them keep working.
    ///
    /// Roles that could log in before reached those databases through `PUBLIC`
    /// too, including ones an operator created by hand (a reporting or BI
    /// role). Each of them is granted `CONNECT` explicitly before `PUBLIC`
    /// loses it, so the lockdown only shuts out sandbox roles. A database
    /// already closed to `PUBLIC` is skipped, so this changes nothing on the
    /// next connect and does not undo grants an operator made since.
    pub fn lockdown_statement(&self) -> String {
        format!(
            "DO $sbx$ DECLARE d record; r record; BEGIN \
             FOR d IN SELECT datname FROM pg_database WHERE {condition} \
             LOOP \
             FOR r IN SELECT rolname FROM pg_roles WHERE rolcanlogin \
             AND left(rolname, {prefix_len}) <> {prefix_literal} \
             AND left(rolname, 3) <> 'pg_' \
             LOOP EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', d.datname, r.rolname); \
             END LOOP; \
             EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', d.datname); \
             END LOOP; \
             REVOKE ALL ON DATABASE \"{database}\" FROM PUBLIC; \
             END $sbx$",
            condition = self.open_databases_condition(),
            prefix_len = ROLE_PREFIX.len(),
            prefix_literal = quote_literal(ROLE_PREFIX),
            database = self.database,
        )
    }

    /// What follows `CREATE`/`ALTER DATABASE`: the databases about to be
    /// closed, then the lockdown itself. One simple-query round trip, run as
    /// one implicit transaction, so the reported list is exactly what changed.
    pub fn finishing_script(&self) -> String {
        format!(
            "{}; {}",
            self.lockdown_targets_query(),
            self.lockdown_statement()
        )
    }

    /// The variables the app reads, under the same names a deployment gets.
    /// `hosts` are the database containers on the sandbox's data network, with
    /// the port the server listens on inside them.
    pub fn environment(&self, hosts: &[(String, u16)]) -> HashMap<String, String> {
        postgres_environment(&self.role, &self.password, &self.database, hosts)
    }
}

/// The plans a sandbox can be given: the reserved name, and the legacy one
/// when it is a valid identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDatabaseCandidates {
    pub reserved: SandboxDatabasePlan,
    pub legacy: Option<SandboxDatabasePlan>,
}

impl SandboxDatabaseCandidates {
    pub fn new(
        service_id: i32,
        namespace: &SandboxNamespace,
        admin_user: &str,
        admin_password: &str,
    ) -> Result<Self, SandboxPlanError> {
        Ok(Self {
            reserved: SandboxDatabasePlan::new(service_id, namespace, admin_user, admin_password)?,
            // Only a project's sandboxes had a legacy name, and only one that
            // names a single project (see
            // `SandboxNamespace::legacy_project_slug`). A legacy name that is
            // not a plain identifier was never provisioned either.
            legacy: namespace.legacy_project_slug().and_then(|slug| {
                SandboxDatabasePlan::legacy(service_id, slug, admin_user, admin_password).ok()
            }),
        })
    }
}

/// Provisioning the sandbox database failed.
#[derive(Debug)]
pub enum SandboxApplyError {
    /// A statement of the plan failed.
    Statement {
        step: &'static str,
        database: String,
        source: tokio_postgres::Error,
    },
    /// The sandbox's database name is taken by a database that is not the
    /// sandbox's. Nothing was changed.
    Conflict(SandboxNamespaceConflict),
}

impl std::fmt::Display for SandboxApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Statement {
                step,
                database,
                source,
            } => write!(
                formatter,
                "could not {step} for the sandbox database '{database}': {source}"
            ),
            Self::Conflict(conflict) => conflict.fmt(formatter),
        }
    }
}

impl std::error::Error for SandboxApplyError {}

/// Run the plan as the administrator, on a connection to any database of the
/// server (the primary, for a cluster), and return the plan that was applied.
///
/// The owners of both candidate names are read first and nothing is changed
/// until [`choose_sandbox_resource`] has picked a database that is missing or
/// already owned by the sandbox's role. A database owned by anyone else is
/// never re-owned, locked down or handed out.
pub async fn apply(
    client: &tokio_postgres::Client,
    candidates: &SandboxDatabaseCandidates,
) -> Result<SandboxDatabasePlan, SandboxApplyError> {
    let failed = |step: &'static str, database: &str| {
        let database = database.to_string();
        move |source: tokio_postgres::Error| SandboxApplyError::Statement {
            step,
            database,
            source,
        }
    };
    let reserved = inspect_database(client, &candidates.reserved).await?;
    let legacy = match &candidates.legacy {
        Some(plan) => inspect_database(client, plan).await?,
        None => ExistingResource::Missing,
    };
    let (plan, create) = match (
        choose_sandbox_resource(&legacy, &reserved),
        &candidates.legacy,
    ) {
        (SandboxResourceChoice::Adopt, _) => (&candidates.reserved, false),
        (SandboxResourceChoice::AdoptLegacy, Some(plan)) => (plan, false),
        // `legacy` is only `Sandbox` when there is a legacy plan.
        (SandboxResourceChoice::AdoptLegacy, None) | (SandboxResourceChoice::Create, _) => {
            (&candidates.reserved, true)
        }
        (SandboxResourceChoice::Refuse { evidence }, _) => {
            return Err(SandboxApplyError::Conflict(
                SandboxNamespaceConflict::NotASandboxDatabase {
                    engine: ENGINE,
                    database: candidates.reserved.database.clone(),
                    evidence,
                },
            ));
        }
    };

    client
        .batch_execute(&plan.role_statement())
        .await
        .map_err(failed("create the role", &plan.database))?;
    if create {
        // Fails, rather than adopting, if something created the database
        // since it was inspected.
        client
            .batch_execute(&plan.create_database_statement())
            .await
            .map_err(failed("create the database", &plan.database))?;
    }
    let messages = client
        .simple_query(&plan.finishing_script())
        .await
        .map_err(failed("restrict who can connect", &plan.database))?;
    let closed: Vec<&str> = messages
        .iter()
        .filter_map(|message| match message {
            tokio_postgres::SimpleQueryMessage::Row(row) => row.try_get(0).ok().flatten(),
            _ => None,
        })
        .collect();
    if !closed.is_empty() {
        info!(
            sandbox_database = %plan.database,
            admin_role = %plan.admin_user,
            databases = ?closed,
            "Closed databases to PUBLIC CONNECT so sandbox roles cannot open them; \
             existing login roles were granted CONNECT explicitly"
        );
    }
    Ok(plan.clone())
}

/// What the database of `plan` is to its role, read without changing anything.
async fn inspect_database(
    client: &tokio_postgres::Client,
    plan: &SandboxDatabasePlan,
) -> Result<ExistingResource, SandboxApplyError> {
    let failed = |source| SandboxApplyError::Statement {
        step: "read the owner",
        database: plan.database.clone(),
        source,
    };
    let owner: Option<String> = client
        .query_opt(SandboxDatabasePlan::DATABASE_OWNER_QUERY, &[&plan.database])
        .await
        .map_err(failed)?
        .map(|row| row.try_get(0))
        .transpose()
        .map_err(failed)?;
    Ok(classify_database(owner.as_deref(), &plan.role))
}

/// Turn a provisioning failure into the manager's error for `service_id`.
fn apply_failure(service_id: i32, place: &str, error: SandboxApplyError) -> ExternalServiceError {
    match error {
        SandboxApplyError::Conflict(conflict) => ExternalServiceError::SandboxNamespaceConflict {
            service_id,
            conflict,
        },
        statement @ SandboxApplyError::Statement { .. } => ExternalServiceError::InternalError {
            reason: format!(
                "Failed to provision the sandbox database on {place} {service_id}: {statement}"
            ),
        },
    }
}

/// The PostgreSQL check a deployment runs before creating or reusing
/// `database` (owned by `owner`, `None` when it does not exist): a name in the
/// sandbox namespace, or a database owned by the sandbox role that name would
/// have (a sandbox's database from before the namespace, such as
/// `<project>_sandbox` for an environment slugged `sandbox`), is refused.
pub fn check_deployment_database(
    database: &str,
    owner: Option<&str>,
) -> Result<(), SandboxNamespaceConflict> {
    let role = sandbox_role_name(database);
    let evidence = match classify_database(owner, &role) {
        ExistingResource::Sandbox => Some(format!("it is owned by the sandbox role '{role}'")),
        ExistingResource::Missing | ExistingResource::Foreign { .. } => None,
    };
    crate::sandbox_engines::check_deployment_database(ENGINE, database, evidence)
}

/// Same variable names as the deployment path, whatever the credentials are.
pub fn postgres_environment(
    user: &str,
    password: &str,
    database: &str,
    hosts: &[(String, u16)],
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("POSTGRES_USER".to_string(), user.to_string());
    env.insert("POSTGRES_PASSWORD".to_string(), password.to_string());
    env.insert("POSTGRES_DB".to_string(), database.to_string());
    env.insert("POSTGRES_NAME".to_string(), database.to_string());
    env.insert("POSTGRES_DATABASE".to_string(), database.to_string());
    if hosts.is_empty() {
        return env;
    }
    // One endpoint everywhere: the URL names the writable member, like
    // `POSTGRES_HOST`/`POSTGRES_PORT`. A multi-host URL is understood by libpq
    // alone; the common Node and Python drivers reject it. The variables are
    // resolved again for every command, so a failover shows up in the next one.
    let authority = format!(
        "{}:{}",
        crate::externalsvc::postgres_cluster::uri_host(&hosts[0].0),
        hosts[0].1
    );
    env.insert(
        "POSTGRES_URL".to_string(),
        format!(
            "postgresql://{}:{}@{authority}/{database}",
            urlencoding::encode(user),
            urlencoding::encode(password),
        ),
    );
    // One endpoint, as a single `host` and `port` pair: a tool that reads
    // these variables dials exactly this and nothing else. A cluster's
    // members listen on different ports, so joining the hosts under one port
    // would send some of them to a port nothing listens on. The caller puts
    // the writable member first.
    env.insert("POSTGRES_HOST".to_string(), hosts[0].0.clone());
    env.insert("POSTGRES_PORT".to_string(), hosts[0].1.to_string());
    env
}

/// PostgreSQL's sandbox access: a database and a role of the sandbox's own on
/// a standalone service or on a cluster's primary.
///
/// Provisions on every call (the statements are idempotent), so a role whose
/// administrator password changed, or a database dropped by hand, is repaired
/// rather than handed out broken. The hosts are the database containers by
/// name: the sandbox resolves them on its data network.
pub struct PostgresSandboxAccess;

#[async_trait::async_trait]
impl SandboxAccessEngine for PostgresSandboxAccess {
    fn service_types(&self) -> &'static [&'static str] {
        &["postgres"]
    }

    async fn containers(
        &self,
        manager: &ExternalServiceManager,
        service: &external_services::Model,
    ) -> Result<Vec<String>, ExternalServiceError> {
        if service.node_id.is_some() {
            return Ok(Vec::new());
        }
        // Every local member, whatever its status now: one that is down when
        // the sandbox connects is the primary after the next failover.
        if service.topology == "cluster" {
            let members = manager.get_service_members(service.id).await?;
            return Ok(members
                .into_iter()
                .filter(|member| {
                    member.node_id.is_none() && !crate::services::is_role_monitor(&member.role)
                })
                .map(|member| member.container_name)
                .collect());
        }
        Ok(vec![service.container_name.clone().unwrap_or_else(|| {
            crate::externalsvc::postgres::postgres_container_name(&service.name)
        })])
    }

    async fn issue(
        &self,
        manager: &ExternalServiceManager,
        request: SandboxAccessRequest<'_>,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let SandboxAccessRequest {
            service,
            namespace,
            project_id,
            parameters,
        } = request;
        let service_id = service.id;
        let param = |key: &str, default: &str| {
            parameters
                .get(key)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        let admin_user = param("username", "postgres");
        let admin_password = param("password", "");
        let candidates =
            SandboxDatabaseCandidates::new(service_id, namespace, &admin_user, &admin_password)
                .map_err(|error| ExternalServiceError::SandboxAccessUnsupported {
                    service_id,
                    service_type: service.service_type.clone(),
                    reason: error.to_string(),
                })?;

        let (plan, hosts) = if service.topology == "cluster" {
            let members = manager.get_service_members(service_id).await?;
            let mut hosts: Vec<(String, u16)> = members
                .iter()
                .filter(|member| {
                    !crate::services::is_role_monitor(&member.role)
                        && member.status == "running"
                        && member.node_id.is_none()
                })
                // A cluster member listens on its own stored port inside the
                // container as well as on the host (the container publishes
                // `port` to `port`), unlike a standalone service's 5432.
                .map(|member| {
                    let port = member
                        .port
                        .and_then(|port| u16::try_from(port).ok())
                        .unwrap_or(5432);
                    (member.container_name.clone(), port)
                })
                .collect();
            if hosts.is_empty() {
                return Err(ExternalServiceError::SandboxAccessUnsupported {
                    service_id,
                    service_type: service.service_type.clone(),
                    reason: "no running data member of this cluster is on the control plane's \
                             Docker network, which is the only place a sandbox can reach"
                        .to_string(),
                });
            }
            let (client, primary) = manager.connect_cluster_primary_client(service_id).await?;
            let plan = apply(&client, &candidates)
                .await
                .map_err(|error| apply_failure(service_id, "cluster", error))?;
            // The writable member first: `POSTGRES_HOST`/`POSTGRES_PORT` name
            // one endpoint, and it should be the one that accepts writes now.
            hosts.sort_by_key(|(container, _)| *container != primary);
            (plan, hosts)
        } else {
            if let crate::services::ServiceExecutionRoute::Remote(node_id) =
                crate::services::service_execution_route(service.node_id)
            {
                return Err(ExternalServiceError::SandboxAccessUnsupported {
                    service_id,
                    service_type: service.service_type.clone(),
                    reason: format!(
                        "the service runs on node {node_id}, and a sandbox can only reach \
                         services on the control plane"
                    ),
                });
            }
            let host = param("host", "localhost");
            let port = parameters
                .get("port")
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value.as_u64().map(|number| number.to_string()))
                })
                .unwrap_or_else(|| "5432".to_string());
            let port_number: u16 =
                port.parse()
                    .map_err(|_| ExternalServiceError::ParameterValidationFailed {
                        service_id,
                        reason: format!("The service's port '{port}' is not a port number"),
                    })?;
            let (client, connection) = tokio_postgres::Config::new()
                .host(&host)
                .port(port_number)
                .user(&admin_user)
                .password(&admin_password)
                .dbname("postgres")
                .connect_timeout(std::time::Duration::from_secs(10))
                .connect(tokio_postgres::NoTls)
                .await
                .map_err(|error| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to connect to PostgreSQL service {} at {}:{} to provision the \
                         sandbox database '{}': {}",
                        service_id, host, port, candidates.reserved.database, error
                    ),
                })?;
            let driver = tokio::spawn(connection);
            let applied = apply(&client, &candidates).await;
            drop(client);
            driver.abort();
            let plan = applied.map_err(|error| apply_failure(service_id, "service", error))?;
            let container = service
                .container_name
                .clone()
                .unwrap_or_else(|| format!("postgres-{}", service.name));
            (plan, vec![(container, 5432)])
        };

        info!(
            service_id,
            project_id = ?project_id,
            database = %plan.database,
            role = %plan.role,
            "Provisioned a sandbox database and scoped role"
        );
        Ok(plan.environment(&hosts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(slug: &str) -> SandboxNamespace {
        SandboxNamespace::project(slug)
    }

    fn plan() -> SandboxDatabasePlan {
        SandboxDatabasePlan::new(3, &project("shop-api"), "postgres", "admin-secret")
            .expect("plan builds")
    }

    #[test]
    fn the_sandbox_database_is_in_the_reserved_namespace() {
        assert_eq!(project("shop-api").database_name(), "_sandbox_shop_api");
        assert_eq!(project("7days").database_name(), "_sandbox_7days");
        assert!(is_plain_identifier(&project("7days").database_name()));
        // The pre-namespace name, still used for a database the sandbox
        // role already owns.
        assert_eq!(legacy_sandbox_database_name("shop-api"), "shop_api_sandbox");
        assert_eq!(legacy_sandbox_database_name("7days"), "db_7days_sandbox");
    }

    #[test]
    fn no_deployment_database_name_is_a_sandbox_database_name() {
        // The reported collision: an environment slugged `sandbox`, and in
        // project mode a project slugged `<project>-sandbox`, are named like
        // the legacy sandbox database. Neither is named like the new one.
        for (project_slug, environment) in [
            ("shop", "sandbox"),
            ("shop-sandbox", ""),
            ("shop", "production"),
            ("7days", "sandbox"),
        ] {
            let deployment = PostgresService::normalize_database_name(&scoped_resource_name(
                project_slug,
                environment,
            ));
            assert_ne!(deployment, project("shop").database_name());
        }
        assert_eq!(
            PostgresService::normalize_database_name(&scoped_resource_name("shop", "sandbox")),
            legacy_sandbox_database_name("shop")
        );
        // A slug never starts with `_`; a deployment whose name lands in the
        // namespace anyway (a custom name, a hand-edited slug) is refused,
        // never created and never given an existing sandbox database.
        let smuggled =
            PostgresService::normalize_database_name(&scoped_resource_name("_sandbox", "shop"));
        assert_eq!(smuggled, project("shop").database_name());
        for owner in [None, Some("postgres"), Some("sbx__sandbox_shop")] {
            assert!(matches!(
                check_deployment_database(&smuggled, owner),
                Err(SandboxNamespaceConflict::ReservedForSandboxes { .. })
            ));
        }
    }

    #[test]
    fn a_long_project_slug_keeps_a_digest_so_two_slugs_never_share_a_database() {
        let first = project(&format!("{}x", "a".repeat(80))).database_name();
        let second = project(&format!("{}y", "a".repeat(80))).database_name();
        assert_ne!(first, second);
        for name in [&first, &second] {
            assert!(name.len() <= IDENTIFIER_MAX, "{name}");
            assert!(is_plain_identifier(name), "{name}");
            assert!(name.starts_with(crate::sandbox_engines::SANDBOX_DATABASE_PREFIX));
            // The role is never hashed, so two databases never share one.
            assert_eq!(sandbox_role_name(name), format!("sbx_{name}"));
            assert!(is_plain_identifier(&sandbox_role_name(name)));
        }
    }

    #[test]
    fn a_database_is_the_sandboxs_only_when_its_role_owns_it() {
        let role = "sbx__sandbox_shop";
        assert_eq!(classify_database(None, role), ExistingResource::Missing);
        assert_eq!(
            classify_database(Some(role), role),
            ExistingResource::Sandbox
        );
        for owner in ["postgres", "app", "sbx_shop_sandbox", "SBX__SANDBOX_SHOP"] {
            match classify_database(Some(owner), role) {
                ExistingResource::Foreign { evidence } => {
                    assert!(evidence.contains(owner), "{evidence}");
                    assert!(evidence.contains(role), "{evidence}");
                }
                other => panic!("{owner}: expected Foreign, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_deployment_is_never_given_a_database_its_sandbox_role_owns() {
        // An environment slugged `sandbox` created after a sandbox was linked
        // asks for `shop_sandbox`, which the legacy sandbox role owns.
        let error = check_deployment_database("shop_sandbox", Some("sbx_shop_sandbox"))
            .expect_err("a sandbox's database must be refused");
        assert!(matches!(
            &error,
            SandboxNamespaceConflict::SandboxOwnedDatabase { database, sandbox_user, .. }
                if database == "shop_sandbox" && sandbox_user == "sbx_shop_sandbox"
        ));
        let message = error.to_string();
        assert!(message.contains("'shop_sandbox'"), "{message}");
        assert!(message.contains("sbx_shop_sandbox"), "{message}");
        // The deployment's own database, or a fresh one, is fine.
        check_deployment_database("shop_sandbox", Some("postgres")).expect("admin-owned");
        check_deployment_database("shop_production", None).expect("new database");
    }

    #[test]
    fn the_role_name_follows_the_database_and_fits_an_identifier() {
        assert_eq!(
            sandbox_role_name("shop_api_sandbox"),
            "sbx_shop_api_sandbox"
        );
        let long = "a".repeat(63);
        let role = sandbox_role_name(&long);
        assert!(role.len() <= IDENTIFIER_MAX, "{role}");
        assert!(is_plain_identifier(&role));
    }

    #[test]
    fn two_long_databases_with_the_same_start_get_different_roles() {
        let first = format!("{}x", "a".repeat(62));
        let second = format!("{}y", "a".repeat(62));
        assert_ne!(sandbox_role_name(&first), sandbox_role_name(&second));
    }

    #[test]
    fn the_password_is_stable_and_depends_on_every_input() {
        let base = derive_role_password("admin-secret", 3, "shop_api_sandbox").expect("derives");
        assert_eq!(
            base,
            derive_role_password("admin-secret", 3, "shop_api_sandbox").expect("derives")
        );
        assert_eq!(base.len(), 64);
        assert!(base.bytes().all(|byte| byte.is_ascii_hexdigit()));
        for other in [
            derive_role_password("other-secret", 3, "shop_api_sandbox"),
            derive_role_password("admin-secret", 4, "shop_api_sandbox"),
            derive_role_password("admin-secret", 3, "other_sandbox"),
        ] {
            assert_ne!(base, other.expect("derives"));
        }
    }

    #[test]
    fn the_role_cannot_do_anything_beyond_using_its_database() {
        let statement = plan().role_statement();
        for attribute in [
            "NOSUPERUSER",
            "NOCREATEDB",
            "NOCREATEROLE",
            "NOREPLICATION",
            "NOBYPASSRLS",
            "CONNECTION LIMIT 20",
        ] {
            assert!(statement.contains(attribute), "{attribute}: {statement}");
        }
        // Created when missing, corrected when present.
        assert!(statement.contains("CREATE ROLE \"sbx__sandbox_shop_api\""));
        assert!(statement.contains("ALTER ROLE \"sbx__sandbox_shop_api\""));
        assert!(statement.contains("rolname = 'sbx__sandbox_shop_api'"));
    }

    #[test]
    fn the_database_is_created_owned_by_the_role_and_never_re_owned() {
        let plan = plan();
        assert_eq!(
            plan.create_database_statement(),
            "CREATE DATABASE \"_sandbox_shop_api\" OWNER \"sbx__sandbox_shop_api\""
        );
        // Nothing in the plan takes an existing database over.
        let every_statement = [
            plan.role_statement(),
            plan.create_database_statement(),
            plan.finishing_script(),
        ]
        .join("\n");
        assert!(!every_statement.contains("OWNER TO"), "{every_statement}");
        // The owner is read with a bound parameter, not spliced in.
        assert!(SandboxDatabasePlan::DATABASE_OWNER_QUERY.contains("datname = $1"));
    }

    #[test]
    fn the_legacy_plan_keeps_the_old_names_and_password() {
        let legacy =
            SandboxDatabasePlan::legacy(3, "shop-api", "postgres", "admin-secret").expect("plan");
        assert_eq!(legacy.database, "shop_api_sandbox");
        assert_eq!(legacy.role, "sbx_shop_api_sandbox");
        // The same derivation as before, so an existing sandbox's
        // credentials keep working.
        assert_eq!(
            legacy.password(),
            derive_role_password("admin-secret", 3, "shop_api_sandbox").expect("derives")
        );
        let candidates =
            SandboxDatabaseCandidates::new(3, &project("shop-api"), "postgres", "admin-secret")
                .expect("ok");
        assert_eq!(candidates.reserved, plan());
        assert_eq!(candidates.legacy, Some(legacy));
    }

    #[test]
    fn only_a_project_with_a_one_to_one_legacy_name_may_adopt_it() {
        let legacy_of = |namespace: &SandboxNamespace| {
            SandboxDatabaseCandidates::new(3, namespace, "postgres", "admin-secret")
                .expect("ok")
                .legacy
                .map(|plan| plan.database)
        };
        // A sandbox linked directly never had a legacy database.
        assert_eq!(legacy_of(&SandboxNamespace::sandbox("sbx_shopapi")), None);
        // `7days` and `db-7days` both had `db_7days_sandbox`, `café` and
        // `cafè` both `caf__sandbox`, and long slugs were truncated or had
        // their role hashed: only a slug that is the sole owner of its legacy
        // name may adopt it (`db-7days`, not `7days`).
        for slug in ["7days", "café", "cafè", &"a".repeat(52)] {
            assert_eq!(legacy_of(&project(slug)), None, "{slug}");
        }
        for (slug, legacy) in [
            ("shop-api", "shop_api_sandbox"),
            ("db-7days", "db_7days_sandbox"),
        ] {
            assert_eq!(legacy_of(&project(slug)).as_deref(), Some(legacy), "{slug}");
        }
        let longest = "a".repeat(51);
        assert_eq!(
            legacy_of(&project(&longest)),
            Some(format!("{longest}_sandbox"))
        );
    }

    #[test]
    fn the_lockdown_keeps_existing_login_roles_connected_before_closing_public() {
        let statement = plan().lockdown_statement();
        let grant = statement
            .find("GRANT CONNECT ON DATABASE %I TO %I")
            .expect("existing roles are granted CONNECT");
        let revoke = statement
            .find("REVOKE CONNECT ON DATABASE %I FROM PUBLIC")
            .expect("PUBLIC loses CONNECT");
        assert!(
            grant < revoke,
            "roles must be granted before PUBLIC is revoked"
        );
        // Only login roles, never sandbox or built-in ones.
        assert!(statement.contains("WHERE rolcanlogin"));
        assert!(statement.contains("left(rolname, 4) <> 'sbx_'"));
        assert!(statement.contains("left(rolname, 3) <> 'pg_'"));
    }

    #[test]
    fn the_lockdown_skips_databases_already_closed_to_public() {
        let plan = plan();
        let open = "(datacl IS NULL OR EXISTS (SELECT 1 FROM aclexplode(datacl) a \
                    WHERE a.grantee = 0 AND a.privilege_type = 'CONNECT'))";
        assert!(plan.lockdown_statement().contains(open));
        assert!(plan.lockdown_targets_query().contains(open));
    }

    #[test]
    fn the_finishing_script_reports_the_same_databases_it_closes() {
        let plan = plan();
        let script = plan.finishing_script();
        assert!(script.starts_with(&plan.lockdown_targets_query()));
        assert!(script.ends_with(&plan.lockdown_statement()));
        assert!(plan
            .lockdown_targets_query()
            .contains(&plan.open_databases_condition()));
        assert!(plan
            .lockdown_statement()
            .contains(&plan.open_databases_condition()));
    }

    #[test]
    fn the_lockdown_closes_the_administrators_databases_but_not_the_maintenance_ones() {
        let statement = plan().lockdown_statement();
        assert!(statement.contains("REVOKE CONNECT ON DATABASE %I FROM PUBLIC"));
        assert!(statement.contains("datname <> 'postgres'"));
        assert!(statement.contains("NOT datistemplate"));
        assert!(statement.contains("rolname = 'postgres'"));
        // The sandbox database is excluded from the loop and revoked on its own.
        assert!(statement.contains("datname <> '_sandbox_shop_api'"));
        assert!(statement.contains("REVOKE ALL ON DATABASE \"_sandbox_shop_api\" FROM PUBLIC"));
    }

    #[test]
    fn an_administrator_name_with_a_quote_cannot_break_out_of_the_literal() {
        let plan =
            SandboxDatabasePlan::new(1, &project("shop"), "o'brien", "pw").expect("plan builds");
        assert!(plan.lockdown_statement().contains("rolname = 'o''brien'"));
    }

    #[test]
    fn a_project_slug_cannot_smuggle_sql_into_an_identifier() {
        // Slugs are normalised to [a-z0-9_] before they get here; the check is
        // the backstop if that ever changes.
        for slug in ["shop\"; DROP DATABASE x; --", "a b", "ünï"] {
            let plan =
                SandboxDatabasePlan::new(1, &project(slug), "postgres", "pw").expect("plan builds");
            assert!(is_plain_identifier(&plan.database), "{}", plan.database);
            assert!(is_plain_identifier(&plan.role), "{}", plan.role);
        }
        assert!(!is_plain_identifier("bad\"name"));
        assert!(!is_plain_identifier("1starts_with_digit"));
        assert!(!is_plain_identifier(""));
        assert!(!is_plain_identifier(&"a".repeat(64)));
    }

    #[test]
    fn the_environment_uses_the_deployment_variable_names() {
        let env = plan().environment(&[("postgres-main".to_string(), 5432)]);
        for name in [
            "POSTGRES_URL",
            "POSTGRES_HOST",
            "POSTGRES_PORT",
            "POSTGRES_DB",
            "POSTGRES_USER",
            "POSTGRES_PASSWORD",
        ] {
            assert!(env.contains_key(name), "missing {name}");
        }
        assert_eq!(env["POSTGRES_DB"], "_sandbox_shop_api");
        assert_eq!(env["POSTGRES_USER"], "sbx__sandbox_shop_api");
        assert_eq!(env["POSTGRES_HOST"], "postgres-main");
        assert_eq!(env["POSTGRES_PORT"], "5432");
        assert!(env["POSTGRES_URL"].starts_with("postgresql://sbx__sandbox_shop_api:"));
        assert!(env["POSTGRES_URL"].ends_with("@postgres-main:5432/_sandbox_shop_api"));
    }

    #[test]
    fn the_environment_never_carries_the_administrator_credentials() {
        let env = plan().environment(&[("postgres-main".to_string(), 5432)]);
        let joined = env.values().cloned().collect::<Vec<_>>().join("\n");
        assert!(!joined.contains("admin-secret"));
        assert_ne!(env["POSTGRES_USER"], "postgres");
    }

    #[test]
    fn a_cluster_url_names_only_the_writable_member() {
        let env = plan().environment(&[
            ("postgres-main-2".to_string(), 6012),
            ("postgres-main-1".to_string(), 6011),
        ]);
        let url = &env["POSTGRES_URL"];
        // One host: the common Node and Python drivers reject a host list.
        assert!(
            url.ends_with("@postgres-main-2:6012/_sandbox_shop_api"),
            "{url}"
        );
        assert!(!url.contains(','), "{url}");
        assert!(!url.contains("target_session_attrs"), "{url}");
    }

    #[test]
    fn host_and_port_name_one_endpoint_even_for_a_cluster() {
        let env = plan().environment(&[
            ("postgres-main-2".to_string(), 6012),
            ("postgres-main-1".to_string(), 6011),
        ]);
        // The pair must be dialable as given: the first member's host with
        // its own port, never a list of hosts under one port.
        assert_eq!(env["POSTGRES_HOST"], "postgres-main-2");
        assert_eq!(env["POSTGRES_PORT"], "6012");
        assert!(!env["POSTGRES_HOST"].contains(','));
    }

    #[test]
    fn without_a_reachable_host_only_the_credentials_are_returned() {
        let env = plan().environment(&[]);
        assert!(!env.contains_key("POSTGRES_URL"));
        assert!(env.contains_key("POSTGRES_PASSWORD"));
    }

    #[test]
    fn the_debug_output_hides_the_password() {
        let plan = plan();
        let shown = format!("{plan:?}");
        assert!(!shown.contains(plan.password()), "{shown}");
        assert!(shown.contains("***"));
    }

    // ---- against a real server -------------------------------------------

    use testcontainers::{
        core::{ContainerPort, Mount, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        database: &str,
    ) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
        let (client, connection) = tokio_postgres::Config::new()
            .host(host)
            .port(port)
            .user(user)
            .password(password)
            .dbname(database)
            .connect(tokio_postgres::NoTls)
            .await?;
        tokio::spawn(connection);
        Ok(client)
    }

    #[tokio::test]
    async fn the_role_owns_its_database_and_can_reach_nothing_else() {
        let docker_up = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_up {
            println!("Docker not available, skipping");
            return;
        }
        let container = GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            // Data in memory: fast, and independent of the Docker disk.
            .with_mount(Mount::tmpfs_mount("/var/lib/postgresql"))
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "admin-secret")
            .start()
            .await
            .expect("the PostgreSQL test container starts");
        let host = container.get_host().await.expect("host").to_string();
        let port = container.get_host_port_ipv4(5432).await.expect("port");

        let admin = connect(&host, port, "postgres", "admin-secret", "postgres")
            .await
            .expect("admin connects");
        // What a deployment's database looks like: created by the
        // administrator, with data in it.
        admin
            .batch_execute("CREATE DATABASE shop_production")
            .await
            .expect("production database");
        let production = connect(&host, port, "postgres", "admin-secret", "shop_production")
            .await
            .expect("admin reaches production");
        production
            .batch_execute("CREATE TABLE orders (id int); INSERT INTO orders VALUES (1)")
            .await
            .expect("production data");

        let candidates =
            SandboxDatabaseCandidates::new(1, &project("shop"), "postgres", "admin-secret")
                .expect("plan");
        let plan = apply(&admin, &candidates).await.expect("first apply");
        assert_eq!(
            plan, candidates.reserved,
            "a new sandbox gets the reserved name"
        );
        // Provisioning runs on every connect: it must be safe to repeat.
        let again = apply(&admin, &candidates).await.expect("second apply");
        assert_eq!(again, plan);

        // The role can build in its own database, migrations included.
        let sandbox = connect(&host, port, &plan.role, plan.password(), &plan.database)
            .await
            .expect("the role connects to its database");
        sandbox
            .batch_execute(
                "CREATE TABLE notes (id serial PRIMARY KEY, body text); \
                 INSERT INTO notes (body) VALUES ('hello'); \
                 CREATE SCHEMA app; CREATE TABLE app.t (id int);",
            )
            .await
            .expect("the role owns its database");
        let rows = sandbox
            .query("SELECT body FROM notes", &[])
            .await
            .expect("reads back");
        assert_eq!(rows.len(), 1);

        // It cannot reach production, wherever it tries.
        let refused = connect(&host, port, &plan.role, plan.password(), "shop_production").await;
        assert!(refused.is_err(), "the role must not connect to production");
        let message = refused
            .err()
            .and_then(|error| error.as_db_error().map(|db| db.message().to_string()))
            .unwrap_or_default();
        assert!(
            message.contains("permission denied for database"),
            "unexpected refusal: {message}"
        );

        // It cannot widen its own rights.
        for statement in [
            "CREATE DATABASE escape".to_string(),
            "CREATE ROLE intruder LOGIN".to_string(),
            format!("ALTER ROLE \"{}\" SUPERUSER", plan.role),
            format!("ALTER DATABASE shop_production OWNER TO \"{}\"", plan.role),
            "SET ROLE postgres".to_string(),
        ] {
            assert!(
                sandbox.batch_execute(&statement).await.is_err(),
                "must be refused: {statement}"
            );
        }

        // The wrong password is refused.
        assert!(connect(&host, port, &plan.role, "guess", &plan.database)
            .await
            .is_err());

        // Production is untouched for the administrator.
        let count: i64 = production
            .query_one("SELECT count(*) FROM orders", &[])
            .await
            .expect("administrator still reads production")
            .get(0);
        assert_eq!(count, 1);
    }

    async fn owner_of(admin: &tokio_postgres::Client, database: &str) -> Option<String> {
        admin
            .query_opt(SandboxDatabasePlan::DATABASE_OWNER_QUERY, &[&database])
            .await
            .expect("owner query")
            .map(|row| row.get(0))
    }

    #[tokio::test]
    async fn a_database_the_sandbox_did_not_create_is_never_taken_over() {
        let docker_up = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_up {
            println!("Docker not available, skipping");
            return;
        }
        let container = GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            // Data in memory: fast, and independent of the Docker disk.
            .with_mount(Mount::tmpfs_mount("/var/lib/postgresql"))
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "admin-secret")
            .start()
            .await
            .expect("the PostgreSQL test container starts");
        let host = container.get_host().await.expect("host").to_string();
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let admin = connect(&host, port, "postgres", "admin-secret", "postgres")
            .await
            .expect("admin connects");

        // 1. A live environment slugged `sandbox`: the deployment created
        //    `shop_sandbox`, the old sandbox name, and keeps data in it.
        admin
            .batch_execute("CREATE DATABASE shop_sandbox")
            .await
            .expect("environment database");
        let environment = connect(&host, port, "postgres", "admin-secret", "shop_sandbox")
            .await
            .expect("admin reaches the environment");
        environment
            .batch_execute("CREATE TABLE orders (id int); INSERT INTO orders VALUES (1)")
            .await
            .expect("environment data");

        let candidates =
            SandboxDatabaseCandidates::new(1, &project("shop"), "postgres", "admin-secret")
                .expect("plan");
        let plan = apply(&admin, &candidates).await.expect("apply");
        assert_eq!(
            plan.database, "_sandbox_shop",
            "the sandbox gets its own name"
        );
        assert_eq!(
            owner_of(&admin, "shop_sandbox").await.as_deref(),
            Some("postgres"),
            "the environment's database keeps its owner"
        );
        let sandbox_role = candidates
            .legacy
            .as_ref()
            .expect("legacy plan")
            .role
            .clone();
        for (role, password) in [
            (plan.role.clone(), plan.password().to_string()),
            (
                sandbox_role.clone(),
                candidates
                    .legacy
                    .as_ref()
                    .expect("legacy")
                    .password()
                    .to_string(),
            ),
        ] {
            assert!(
                connect(&host, port, &role, &password, "shop_sandbox")
                    .await
                    .is_err(),
                "{role} must not reach the environment's database"
            );
        }
        // The deployment keeps using its database.
        check_deployment_database(
            "shop_sandbox",
            owner_of(&admin, "shop_sandbox").await.as_deref(),
        )
        .expect("the environment's own database is not a sandbox's");
        let count: i64 = environment
            .query_one("SELECT count(*) FROM orders", &[])
            .await
            .expect("environment data intact")
            .get(0);
        assert_eq!(count, 1);

        // 2. Something made by hand under the reserved name is refused, and
        //    left exactly as it was.
        admin
            .batch_execute("CREATE DATABASE _sandbox_other")
            .await
            .expect("hand-made database");
        let other =
            SandboxDatabaseCandidates::new(1, &project("other"), "postgres", "admin-secret")
                .expect("plan");
        let error = apply(&admin, &other).await.expect_err("must be refused");
        let SandboxApplyError::Conflict(conflict) = &error else {
            panic!("expected a conflict, got {error}");
        };
        assert!(matches!(
            conflict,
            SandboxNamespaceConflict::NotASandboxDatabase { database, .. }
                if database == "_sandbox_other"
        ));
        assert!(error.to_string().contains("'postgres'"), "{error}");
        assert_eq!(
            owner_of(&admin, "_sandbox_other").await.as_deref(),
            Some("postgres")
        );

        // 3. A sandbox from before the namespace (its role owns
        //    `legacy_sandbox`) keeps its database and its data.
        let legacy =
            SandboxDatabaseCandidates::new(1, &project("legacy"), "postgres", "admin-secret")
                .expect("plan");
        let legacy_plan = legacy.legacy.clone().expect("legacy plan");
        admin
            .batch_execute(&legacy_plan.role_statement())
            .await
            .expect("legacy role");
        admin
            .batch_execute(&legacy_plan.create_database_statement())
            .await
            .expect("legacy database");
        let applied = apply(&admin, &legacy).await.expect("legacy apply");
        assert_eq!(applied, legacy_plan, "the legacy database is kept");
        assert_eq!(
            owner_of(&admin, "_sandbox_legacy").await,
            None,
            "no second database is created"
        );
        let sandbox = connect(
            &host,
            port,
            &applied.role,
            applied.password(),
            "legacy_sandbox",
        )
        .await
        .expect("the legacy role connects");
        sandbox
            .batch_execute("CREATE TABLE t (id int)")
            .await
            .expect("the legacy role owns its database");

        // 4. The reverse direction: an environment slugged `sandbox` created
        //    after that sandbox asks for `legacy_sandbox`, and is refused.
        let refused = check_deployment_database(
            "legacy_sandbox",
            owner_of(&admin, "legacy_sandbox").await.as_deref(),
        )
        .expect_err("a deployment must not be given a sandbox's database");
        assert!(matches!(
            refused,
            SandboxNamespaceConflict::SandboxOwnedDatabase { .. }
        ));
    }
}
