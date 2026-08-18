//! Resource execution phase for platform imports.
//!
//! Importers describe *what* exists on the source platform; this module makes
//! it real on the temps side, source-agnostically, around the importer's own
//! `execute()`:
//!
//! 1. **Services** — every `ServicePlan` with `action = Create` becomes a
//!    temps-managed service via [`ExternalServiceManager`].
//! 2. **Data population** — services whose plan carries a reachable
//!    `source_url` get their data copied by a one-off dump/restore container
//!    (`pg_dump | psql`, `mysqldump | mysql`, `mongodump | mongorestore`)
//!    running on the host network so it can reach both the source machine and
//!    the newly created local service.
//! 3. **Domains** — every `DomainPlan` with `action = Import` becomes a
//!    custom domain on the imported environment (certificates are issued
//!    on-demand once the user cuts DNS over).
//! 4. **Env rewriting** — env var values referencing the source platform are
//!    rewritten before the project is created: source database URLs point at
//!    the new managed service, and source-generated domains (sslip.io,
//!    traefik.me, CapRover subdomains) point at the environment's preview
//!    hostname.
//!
//! Every step reports an honest [`StepResult`] — a failed population or a
//! duplicate domain never aborts the rest of the import.

use bollard::Docker;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use temps_core::public_hostname::PublicHostnameStrategy;
use temps_import_types::{CreatedResource, DomainAction, ImportPlan, ServiceAction, StepResult};
use temps_projects::services::CustomDomainService;
use temps_providers::externalsvc::ServiceType;
use temps_providers::services::{
    CreateExternalServiceRequest, ExternalServiceInfo, ExternalServiceManager,
};
use tracing::{info, warn};

/// Key inside `ServicePlan.parameters` holding the reachable source
/// connection URL (set by the platform importers at plan time).
pub const SOURCE_URL_PARAM: &str = "source_url";

/// Hard cap on how long a data-transfer container may run, in production —
/// deliberately NOT `#[cfg(test)]`-shrunk (unlike `TRANSFER_TIMEOUT` below),
/// so a same-crate test can assert against the real value regardless of
/// which cfg it's compiled under. Cross-checked by
/// `crates/temps-proxy/src/proxy.rs`'s `CONSOLE_IO_TIMEOUT_SECS` (a
/// different crate, so it duplicates this number with a comment pointing
/// back here) — the invariant test at the bottom of this file is the
/// source of truth; update both if this changes.
pub(crate) const TRANSFER_TIMEOUT_PROD: Duration = Duration::from_secs(30 * 60);

/// Hard cap on how long a data-transfer container may run. A dump/restore
/// against an unreachable or very slow source database must not hang a
/// Tokio worker forever — a few of these on a small (cpx22-class) box would
/// starve every other import. Real transfers on a reachable database finish
/// in seconds to minutes; shrinks to a few seconds under `cfg(test)` so
/// the timeout path itself can be exercised against a real container
/// without a real 30-minute wait.
#[cfg(not(test))]
const TRANSFER_TIMEOUT: Duration = TRANSFER_TIMEOUT_PROD;
#[cfg(test)]
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);

/// A managed service created during import, with everything needed to
/// populate it and rewrite env vars.
pub struct CreatedServiceRecord {
    /// Name from the plan (matches `ServicePlan.name`)
    pub plan_name: String,
    /// Normalized plan service type ("postgres", "mysql", ...)
    pub plan_type: String,
    /// The created temps service
    pub info: ExternalServiceInfo,
    /// Locally reachable connection URL of the new service
    pub local_url: Option<String>,
    /// Reachable connection URL on the source platform (for population)
    pub source_url: Option<String>,
}

/// Executes the platform-generic parts of an import plan.
pub struct ResourceExecutor {
    external_services: Arc<ExternalServiceManager>,
    custom_domains: Arc<CustomDomainService>,
    docker: Arc<Docker>,
}

impl ResourceExecutor {
    pub fn new(
        external_services: Arc<ExternalServiceManager>,
        custom_domains: Arc<CustomDomainService>,
        docker: Arc<Docker>,
    ) -> Self {
        Self {
            external_services,
            custom_domains,
            docker,
        }
    }

    /// Map a plan's service type string to a temps [`ServiceType`].
    /// Returns `None` for types temps has no managed equivalent for.
    pub fn map_service_type(plan_type: &str) -> Option<ServiceType> {
        match plan_type {
            "postgres" | "postgresql" => Some(ServiceType::Postgres),
            // temps' managed MySQL-compatible service is MariaDB
            "mysql" | "mariadb" => Some(ServiceType::Mariadb),
            "mongodb" | "mongo" => Some(ServiceType::Mongodb),
            "redis" | "keydb" | "dragonfly" => Some(ServiceType::Redis),
            _ => None,
        }
    }

    /// Create every `action = Create` service in the plan.
    ///
    /// `project_name` namespaces the created services: without it, importing
    /// two projects whose databases share a name (or retrying an import)
    /// collides on the same managed-service container, and the second import
    /// silently talks to the first one's database.
    pub async fn create_services(
        &self,
        plan: &ImportPlan,
        project_name: &str,
    ) -> (
        Vec<StepResult>,
        Vec<CreatedServiceRecord>,
        Vec<CreatedResource>,
    ) {
        let mut steps = Vec::new();
        let mut created = Vec::new();
        let mut resources = Vec::new();

        for service_plan in plan
            .services
            .iter()
            .filter(|s| s.action == ServiceAction::Create)
        {
            let step_id = format!("create-service-{}", sanitize_slug(&service_plan.name));
            let started = std::time::Instant::now();

            let Some(service_type) = Self::map_service_type(&service_plan.service_type) else {
                steps.push(StepResult {
                    step_id,
                    step_title: format!("Create managed service '{}'", service_plan.name),
                    success: true,
                    skipped: true,
                    message: format!(
                        "temps has no managed equivalent for '{}' — create a replacement manually",
                        service_plan.service_type
                    ),
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
                continue;
            };

            // SSRF guard. `source_url` is a *separate* value from the
            // importer's `base_url` (which is validated in the orchestrator):
            // it comes out of the remote platform's API response — Coolify,
            // for example, returns `external_db_url` verbatim whenever the
            // database `is_public`. `populate_services` later starts an
            // official database client container with `network_mode=host` and
            // passes this straight to pg_dump/mariadb-dump/mongodump, so an
            // attacker-controlled source platform could point Temps at a
            // loopback, RFC 1918 or control-plane-internal address and have
            // its contents copied into a project they own.
            //
            // Drop the URL rather than failing the whole import: the service
            // is still created, and `populate_one_service` already has a
            // "source not reachable — copy the data manually" path for a
            // missing source_url, which is exactly the right outcome here.
            //
            // Resolved, not just parsed. The attacker here *is* the source
            // platform, so they also control the DNS for any hostname they
            // return: a literal-only check is defeated by one A record pointing
            // `db.attacker.tld` at 127.0.0.1 or 169.254.169.254.
            let candidate_source_url = service_plan
                .parameters
                .get(SOURCE_URL_PARAM)
                .and_then(|v| v.as_str());
            let source_url = match candidate_source_url {
                None => None,
                Some(url) => {
                    match temps_core::url_validation::validate_external_database_url_async(url)
                        .await
                    {
                        Ok(_) => Some(url),
                        Err(e) => {
                            warn!(
                                service = %service_plan.name,
                                error = %e,
                                source_url = %temps_core::url_validation::redact_url_password(url),
                                "Refusing to use the source platform's database URL for automatic \
                                 data copy: it does not point at a reachable public address. The \
                                 service will be created empty; copy its data manually."
                            );
                            None
                        }
                    }
                }
            };
            let service_name = format!(
                "{}-{}",
                sanitize_slug(project_name),
                sanitize_slug(&service_plan.name)
            );
            let request = CreateExternalServiceRequest {
                name: service_name,
                service_type,
                version: service_plan.version.clone(),
                parameters: service_parameters(&service_type, &service_plan.name, source_url),
                node_id: None,
                topology: "standalone".to_string(),
                members: vec![],
            };

            match self.external_services.create_service(request).await {
                Ok(info) => {
                    let local_url = self.local_dsn(info.id, &service_plan.service_type).await;
                    if local_url.is_none() {
                        warn!(
                            "Created service {} but could not build its local connection URL — data population and env rewriting are skipped for it",
                            info.id
                        );
                    }

                    let source_url = source_url.map(|s| s.to_string());

                    resources.push(CreatedResource {
                        resource_type: "service".to_string(),
                        resource_id: info.id,
                        resource_name: info.name.clone(),
                    });
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Create managed service '{}'", service_plan.name),
                        success: true,
                        skipped: false,
                        message: format!(
                            "Created temps-managed {} service '{}' (id {})",
                            service_plan.service_type, info.name, info.id
                        ),
                        created_resources: vec![CreatedResource {
                            resource_type: "service".to_string(),
                            resource_id: info.id,
                            resource_name: info.name.clone(),
                        }],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                    created.push(CreatedServiceRecord {
                        plan_name: service_plan.name.clone(),
                        plan_type: service_plan.service_type.clone(),
                        info,
                        local_url,
                        source_url,
                    });
                }
                Err(e) => {
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Create managed service '{}'", service_plan.name),
                        success: false,
                        skipped: false,
                        message: format!(
                            "Failed to create managed {} service '{}': {}",
                            service_plan.service_type, service_plan.name, e
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
            }
        }

        (steps, created, resources)
    }

    /// Build a locally reachable, correctly encoded connection URL for a
    /// created service. `get_local_address` yields only `host:port`; the
    /// credentials come from the service's (decrypted) config parameters.
    async fn local_dsn(&self, service_id: i32, plan_type: &str) -> Option<String> {
        let model = self.external_services.get_service(service_id).await.ok()?;
        let config = self
            .external_services
            .get_service_config(service_id)
            .await
            .ok()?;
        let address = self.external_services.get_local_address(model).await.ok()?;
        let (host, port) = address.rsplit_once(':')?;
        // Published service ports bind IPv4 only; "localhost" resolves to ::1
        // first inside the transfer container and the connection is refused
        // before IPv4 is ever tried.
        let host = if host == "localhost" {
            "127.0.0.1"
        } else {
            host
        };

        let parameter = |key: &str| {
            config
                .parameters
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        let scheme = match plan_type {
            "postgres" | "postgresql" => "postgres",
            "mysql" | "mariadb" => "mysql",
            "mongodb" | "mongo" => "mongodb",
            "redis" | "keydb" | "dragonfly" => "redis",
            _ => return None,
        };

        // Percent-encode credentials ourselves: url::Url's set_password leaves
        // a literal '%' untouched (it assumes userinfo is already encoded), so
        // a generated password containing '%' would produce a DSN that libpq
        // rejects as an invalid percent-encoded token.
        let userinfo = match (parameter("username"), parameter("password")) {
            (Some(username), Some(password)) => format!(
                "{}:{}@",
                percent_encode_userinfo(&username),
                percent_encode_userinfo(&password)
            ),
            (Some(username), None) => format!("{}@", percent_encode_userinfo(&username)),
            _ => String::new(),
        };
        let database = parameter("database")
            .map(|db| format!("/{}", db))
            .unwrap_or_default();
        Some(format!(
            "{}://{}{}:{}{}",
            scheme, userinfo, host, port, database
        ))
    }

    /// Copy data into each created service that has a reachable source URL,
    /// using a one-off container on the host network (so both the external
    /// source machine and the local service port are reachable).
    ///
    /// Runs every service's transfer concurrently rather than one at a time:
    /// with N services each individually bounded by `TRANSFER_TIMEOUT`, a
    /// sequential loop's worst case is `N * TRANSFER_TIMEOUT`, which grows
    /// unboundedly with the number of services and can exceed the proxy's
    /// request timeout even though each transfer is itself bounded. Running
    /// them concurrently keeps the worst case at a single `TRANSFER_TIMEOUT`
    /// regardless of how many services the import creates.
    pub async fn populate_services(&self, created: &[CreatedServiceRecord]) -> Vec<StepResult> {
        let futures = created
            .iter()
            .map(|record| self.populate_one_service(record));
        futures_util::future::join_all(futures).await
    }

    async fn populate_one_service(&self, record: &CreatedServiceRecord) -> StepResult {
        let step_id = format!("populate-service-{}", sanitize_slug(&record.plan_name));
        let started = std::time::Instant::now();

        let (Some(source_url), Some(local_url)) =
            (record.source_url.as_deref(), record.local_url.as_deref())
        else {
            return StepResult {
                step_id,
                step_title: format!("Copy data into '{}'", record.plan_name),
                success: true,
                skipped: true,
                message: format!(
                    "Data for '{}' was not copied automatically — the source database is not reachable from this server (see the plan's data implications for the manual dump/restore path)",
                    record.plan_name
                ),
                created_resources: vec![],
                duration_seconds: started.elapsed().as_secs_f64(),
            };
        };

        let Some((image, command)) = dump_restore_command(&record.plan_type) else {
            return StepResult {
                step_id,
                step_title: format!("Copy data into '{}'", record.plan_name),
                success: true,
                skipped: true,
                message: format!(
                    "Automatic data copy is not supported for {} — copy the data manually",
                    record.plan_type
                ),
                created_resources: vec![],
                duration_seconds: started.elapsed().as_secs_f64(),
            };
        };

        match self
            .run_transfer_container(&image, &command, source_url, local_url)
            .await
        {
            Ok(()) => {
                info!(
                    "Populated imported service '{}' from source platform",
                    record.plan_name
                );
                StepResult {
                    step_id,
                    step_title: format!("Copy data into '{}'", record.plan_name),
                    success: true,
                    skipped: false,
                    message: format!(
                        "Copied data from the source platform into managed service '{}'",
                        record.info.name
                    ),
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                }
            }
            Err(e) => StepResult {
                step_id,
                step_title: format!("Copy data into '{}'", record.plan_name),
                success: false,
                skipped: false,
                message: format!(
                    "Data copy into '{}' failed: {} — the service was created empty; run the dump/restore from the plan manually",
                    record.info.name, e
                ),
                created_resources: vec![],
                duration_seconds: started.elapsed().as_secs_f64(),
            },
        }
    }

    /// Run a one-off transfer container: `<command>` with SRC/DST env vars.
    async fn run_transfer_container(
        &self,
        image: &str,
        command: &str,
        source_url: &str,
        local_url: &str,
    ) -> Result<(), String> {
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptions, RemoveContainerOptions,
            StartContainerOptions,
        };
        use futures_util::StreamExt;

        // Ensure the image exists (no-op when already pulled)
        let mut pull = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = pull.next().await {
            if let Err(e) = item {
                return Err(format!("failed to pull transfer image '{}': {}", image, e));
            }
        }

        let name = format!(
            "temps-import-transfer-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let container = self
            .docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        command.to_string(),
                    ]),
                    env: Some(vec![
                        format!("SRC={}", source_url),
                        format!("DST={}", local_url),
                    ]),
                    host_config: Some(HostConfig {
                        network_mode: Some("host".to_string()),
                        auto_remove: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("failed to create transfer container: {}", e))?;

        self.docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| format!("failed to start transfer container: {}", e))?;

        // Wait for completion, bounded by TRANSFER_TIMEOUT — an unreachable
        // or hung source database must not tie up a Tokio worker forever.
        let status = match wait_for_container(&self.docker, &container.id, TRANSFER_TIMEOUT).await {
            Ok(status) => status,
            Err(e) => return Err(e),
        };

        // Capture the tail of the logs for the error message before removal
        let logs = container_log_tail(&self.docker, &container.id).await;
        let _ = self
            .docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if status == 0 {
            Ok(())
        } else {
            Err(format!(
                "transfer exited with status {}: {}",
                status,
                logs.chars().take(500).collect::<String>()
            ))
        }
    }

    /// Create every `action = Import` domain on the imported environment.
    pub async fn create_domains(
        &self,
        plan: &ImportPlan,
        project_id: i32,
        environment_id: i32,
    ) -> (Vec<StepResult>, Vec<CreatedResource>) {
        let mut steps = Vec::new();
        let mut resources = Vec::new();

        for domain_plan in plan
            .domains
            .iter()
            .filter(|d| d.action == DomainAction::Import)
        {
            let step_id = format!("import-domain-{}", sanitize_slug(&domain_plan.domain));
            let started = std::time::Instant::now();

            match self
                .custom_domains
                .create_custom_domain(
                    project_id,
                    environment_id,
                    domain_plan.domain.clone(),
                    domain_plan.redirect_to.clone(),
                    domain_plan.status_code,
                    None,
                    None,
                )
                .await
            {
                Ok(model) => {
                    resources.push(CreatedResource {
                        resource_type: "domain".to_string(),
                        resource_id: model.id,
                        resource_name: domain_plan.domain.clone(),
                    });
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Import domain '{}'", domain_plan.domain),
                        success: true,
                        skipped: false,
                        message: format!(
                            "Registered '{}' on the imported environment — point its DNS at this server to finish the cutover (the certificate is issued automatically on first request)",
                            domain_plan.domain
                        ),
                        created_resources: vec![CreatedResource {
                            resource_type: "domain".to_string(),
                            resource_id: model.id,
                            resource_name: domain_plan.domain.clone(),
                        }],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
                Err(e) => {
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Import domain '{}'", domain_plan.domain),
                        success: false,
                        skipped: false,
                        message: format!(
                            "Failed to register domain '{}': {} — add it manually on the environment",
                            domain_plan.domain, e
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
            }
        }

        (steps, resources)
    }
}

/// Wait for `container_id` to finish, bounded by `timeout`. On success
/// returns the exit status code. On timeout or a wait-stream error, force-
/// removes the container and returns a descriptive error including the log
/// tail — callers must not leak a hung container back to Docker.
async fn wait_for_container(
    docker: &Docker,
    container_id: &str,
    timeout: Duration,
) -> Result<i64, String> {
    use bollard::query_parameters::{RemoveContainerOptions, WaitContainerOptions};
    use futures_util::StreamExt;

    let mut wait = docker.wait_container(container_id, None::<WaitContainerOptions>);
    // bollard's wait errors on nonzero exit codes with an often-empty
    // message — treat it as a failed status and let the log tail explain.
    match tokio::time::timeout(timeout, wait.next()).await {
        Ok(Some(Ok(result))) => Ok(result.status_code),
        Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => Ok(code),
        Ok(Some(Err(e))) => {
            let logs = container_log_tail(docker, container_id).await;
            let _ = docker
                .remove_container(
                    container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            Err(format!(
                "transfer container failed: {} — {}",
                e,
                logs.chars().take(400).collect::<String>()
            ))
        }
        Ok(None) => Ok(-1),
        Err(_) => {
            let logs = container_log_tail(docker, container_id).await;
            let _ = docker
                .remove_container(
                    container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            Err(format!(
                "transfer timed out after {}s — the source database may be \
                 unreachable or too slow to respond; check network connectivity \
                 and consider running the dump/restore manually. Log tail: {}",
                timeout.as_secs(),
                logs.chars().take(400).collect::<String>()
            ))
        }
    }
}

async fn container_log_tail(docker: &Docker, container_id: &str) -> String {
    use bollard::query_parameters::LogsOptionsBuilder;
    use futures_util::StreamExt;

    let mut stream = docker.logs(
        container_id,
        Some(
            LogsOptionsBuilder::new()
                .stdout(true)
                .stderr(true)
                .tail("20")
                .build(),
        ),
    );
    let mut output = String::new();
    while let Some(Ok(chunk)) = stream.next().await {
        output.push_str(&chunk.to_string());
    }
    output
}

/// Required creation parameters per service type, derived from the source
/// connection URL when available so the imported service matches what the
/// application expects (database name, user). Passwords are always
/// auto-generated by the service manager.
fn service_parameters(
    service_type: &ServiceType,
    plan_name: &str,
    source_url: Option<&str>,
) -> HashMap<String, serde_json::Value> {
    let mut parameters = HashMap::new();

    // The port must be chosen here and persisted with the service: providers
    // auto-assign a free port when none is configured, but that assignment is
    // never written back — every later config read (local address, backups)
    // would re-roll a different port than the one the container publishes.
    let default_port: Option<u16> = match service_type {
        ServiceType::Postgres => Some(5432),
        ServiceType::Mariadb => Some(3306),
        ServiceType::Mongodb => Some(27017),
        ServiceType::Redis => Some(6379),
        _ => None,
    };
    if let Some(port) = default_port.map(find_free_local_port) {
        parameters.insert("port".to_string(), serde_json::json!(port.to_string()));
    }

    if !matches!(
        service_type,
        ServiceType::Postgres | ServiceType::Mariadb | ServiceType::Mongodb
    ) {
        return parameters;
    }

    let parsed = source_url.and_then(|u| url::Url::parse(u).ok());
    let database = parsed
        .as_ref()
        .map(|u| u.path().trim_start_matches('/').to_string())
        .filter(|db| !db.is_empty())
        .unwrap_or_else(|| sanitize_db_identifier(plan_name));
    let username = parsed
        .as_ref()
        .map(|u| u.username().to_string())
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "app".to_string());

    parameters.insert("database".to_string(), serde_json::json!(database));
    parameters.insert("username".to_string(), serde_json::json!(username));
    parameters
}

/// First host port from `start` upward that accepts a local bind. Falls back
/// to `start` when nothing in the probed range is free — the provider's own
/// port-conflict retry will then reassign at container-create time.
fn find_free_local_port(start: u16) -> u16 {
    (start..start.saturating_add(200))
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .unwrap_or(start)
}

/// A safe database identifier from an arbitrary service name
fn sanitize_db_identifier(name: &str) -> String {
    let cleaned: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Percent-encode a DSN userinfo component. Everything outside the RFC 3986
/// unreserved set is encoded — including '%' itself, which the url crate's
/// userinfo setters pass through unchanged.
fn percent_encode_userinfo(raw: &str) -> String {
    let mut encoded = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

/// Image + shell command per service type for the one-off data transfer.
/// The command reads `$SRC` (source platform URL) and `$DST` (new local URL).
fn dump_restore_command(plan_type: &str) -> Option<(String, String)> {
    // Each command first waits for the freshly created managed service to
    // accept connections — the database container may still be initializing
    // when the transfer starts — then runs the dump/restore.
    match plan_type {
        "postgres" | "postgresql" => Some((
            "postgres:16-alpine".to_string(),
            "for i in $(seq 1 45); do pg_isready -d \"$DST\" >/dev/null 2>&1 && break; sleep 2; done; pg_dump --no-owner --no-privileges \"$SRC\" | psql \"$DST\"".to_string(),
        )),
        "mysql" | "mariadb" => Some((
            "mariadb:11".to_string(),
            "for i in $(seq 1 45); do mariadb --skip-ssl \"--uri=$DST\" -e 'SELECT 1' >/dev/null 2>&1 && break; sleep 2; done; mariadb-dump --skip-ssl \"--uri=$SRC\" | mariadb \"--uri=$DST\"".to_string(),
        )),
        "mongodb" | "mongo" => Some((
            "mongo:7".to_string(),
            "for i in $(seq 1 45); do mongosh \"$DST\" --quiet --eval 'db.runCommand({ping:1})' >/dev/null 2>&1 && break; sleep 2; done; mongodump --uri=\"$SRC\" --archive | mongorestore --uri=\"$DST\" --archive"
                .to_string(),
        )),
        _ => None,
    }
}

/// Compute the environment's preview hostname (`{env-subdomain}.{preview_domain}`).
pub fn preview_hostname(preview_domain: &str, environment_subdomain: &str) -> String {
    PublicHostnameStrategy::Standard.environment_hostname(preview_domain, environment_subdomain)
}

/// Build env-var value rewrites for an import:
/// - reachable source database URLs → the new managed service's local URL
/// - source-generated domains (the plan's skipped domains) → the preview hostname
pub fn build_env_rewrites(
    plan: &ImportPlan,
    created: &[CreatedServiceRecord],
    preview_host: &str,
) -> Vec<(String, String)> {
    let mut rewrites = Vec::new();

    for record in created {
        if let (Some(source_url), Some(local_url)) = (&record.source_url, &record.local_url) {
            rewrites.push((source_url.clone(), local_url.clone()));
        }
    }

    for domain_plan in plan
        .domains
        .iter()
        .filter(|d| d.action == DomainAction::Skip)
    {
        if domain_plan.domain != preview_host {
            rewrites.push((domain_plan.domain.clone(), preview_host.to_string()));
        }
    }

    rewrites
}

/// Annotate skipped (source-generated, IP-tied) domains with the temps-side
/// address that replaces them, so the plan answers "where will my app be
/// reachable instead?" before the user approves it.
pub fn annotate_skipped_domains(plan: &mut ImportPlan, preview_host: &str) -> usize {
    let mut annotated = 0;
    for domain_plan in plan
        .domains
        .iter_mut()
        .filter(|d| d.action == DomainAction::Skip)
    {
        domain_plan.replacement = Some(preview_host.to_string());
        domain_plan.action_description = format!(
            "'{}' embeds the source server's IP and would keep pointing at the old machine — on temps this app will be served at '{}' instead",
            domain_plan.domain, preview_host
        );
        annotated += 1;
    }
    annotated
}

/// Apply rewrites to the plan's env vars (substring replacement, longest
/// patterns first so full URLs win over their embedded hostnames).
/// Returns the number of variables whose value changed.
pub fn apply_env_rewrites(plan: &mut ImportPlan, rewrites: &[(String, String)]) -> usize {
    let mut ordered: Vec<&(String, String)> = rewrites.iter().collect();
    ordered.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));

    let mut changed = 0;
    let deployments =
        std::iter::once(&mut plan.deployment).chain(plan.additional_deployments.iter_mut());
    for deployment in deployments {
        for env_var in &mut deployment.env_vars {
            let original = env_var.value.clone();
            for (from, to) in &ordered {
                if env_var.value.contains(from.as_str()) {
                    env_var.value = env_var.value.replace(from.as_str(), to);
                }
            }
            if env_var.value != original {
                env_var.source_description = Some(format!(
                    "{} (rewritten for temps during import)",
                    env_var.source_description.as_deref().unwrap_or("imported")
                ));
                changed += 1;
            }
        }
    }
    changed
}

fn sanitize_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_import_types::plan::{
        DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration, EnvironmentVariable,
        NetworkConfiguration, NetworkMode, PlanComplexity, PlanMetadata, ProjectConfiguration,
        ProjectType, ResourceLimits,
    };
    use temps_import_types::{DomainPlan, MigrationSummary, ResourceCounts, RiskLevel};

    /// `execute_import`'s real worst case is `TRANSFER_TIMEOUT_PROD` (service
    /// transfers now run concurrently, so N services no longer multiply this)
    /// plus deploy-and-verify's `TRIGGER_GRACE + DEPLOY_TIMEOUT + HTTP_TIMEOUT`
    /// -- all inside the one HTTP request `crates/temps-proxy`'s console
    /// timeout bounds. This crate owns all four constants, so *this* test
    /// (not `temps-proxy`'s, which can't depend on this crate and must
    /// duplicate the numbers) is what actually catches drift: if any of
    /// these four change enough to threaten the invariant, this test fails
    /// here, at the source, instead of silently only in a hardcoded copy
    /// three commits and one crate away.
    ///
    /// If this ever fails: also update `CONSOLE_IO_TIMEOUT_SECS` and its own
    /// (necessarily duplicated) test in `crates/temps-proxy/src/proxy.rs`.
    #[test]
    fn worst_case_execute_duration_fits_under_the_documented_console_timeout() {
        use crate::services::deployment_verifier::{DEPLOY_TIMEOUT, HTTP_TIMEOUT, TRIGGER_GRACE};

        const CONSOLE_IO_TIMEOUT_SECS: u64 = 3600; // crates/temps-proxy/src/proxy.rs

        let worst_case = TRANSFER_TIMEOUT_PROD + TRIGGER_GRACE + DEPLOY_TIMEOUT + HTTP_TIMEOUT;

        assert!(
            worst_case.as_secs() < CONSOLE_IO_TIMEOUT_SECS,
            "worst-case import execute duration ({}s) must stay under the proxy's \
             console timeout ({CONSOLE_IO_TIMEOUT_SECS}s), or the exact \
             'import succeeds server-side, browser sees a dead connection' bug \
             this timeout exists to prevent reopens",
            worst_case.as_secs()
        );
    }

    #[test]
    fn userinfo_encoding_covers_url_hostile_password_chars() {
        // '%' is the case url::Url::set_password gets wrong — it must be
        // encoded so libpq does not see an invalid percent-escape.
        assert_eq!(
            percent_encode_userinfo("=&v1d%ghuyL@i0^S3"),
            "%3D%26v1d%25ghuyL%40i0%5ES3"
        );
        assert_eq!(percent_encode_userinfo("plain-User_1.~"), "plain-User_1.~");
    }

    pub(super) fn plan_with(env: Vec<(&str, &str)>, skipped_domain: &str) -> ImportPlan {
        ImportPlan {
            version: "1.0".to_string(),
            source: "coolify".to_string(),
            source_id: "x".to_string(),
            project: ProjectConfiguration {
                name: "lab".to_string(),
                slug: "lab".to_string(),
                project_type: ProjectType::Git,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "lab".to_string(),
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
            },
            deployment: DeploymentConfiguration {
                image: "x".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: env
                    .into_iter()
                    .map(|(k, v)| EnvironmentVariable {
                        key: k.to_string(),
                        value: v.to_string(),
                        is_secret: false,
                        source_description: None,
                    })
                    .collect(),
                ports: vec![],
                volumes: vec![],
                network: NetworkConfiguration {
                    mode: NetworkMode::Bridge,
                    hostname: None,
                    dns_servers: vec![],
                },
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
                command: None,
                entrypoint: None,
                working_dir: None,
                health_check: None,
                git: None,
            },
            services: vec![],
            domains: vec![DomainPlan {
                domain: skipped_domain.to_string(),
                environment: "production".to_string(),
                redirect_to: None,
                status_code: None,
                action: DomainAction::Skip,
                action_description: String::new(),
                replacement: None,
            }],
            additional_deployments: vec![],
            steps: vec![],
            summary: MigrationSummary {
                headline: String::new(),
                overall_risk: RiskLevel::Low,
                resource_counts: ResourceCounts::default(),
                critical_warnings: vec![],
                manual_actions_required: vec![],
                unsupported_features: vec![],
            },
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: "test".to_string(),
                complexity: PlanComplexity::Low,
                warnings: vec![],
            },
            cost_analysis: None,
        }
    }

    fn record(source: &str, local: &str) -> CreatedServiceRecord {
        CreatedServiceRecord {
            plan_name: "lab-db".to_string(),
            plan_type: "postgres".to_string(),
            info: ExternalServiceInfo {
                id: 1,
                name: "lab-db".to_string(),
                service_type: ServiceType::Postgres,
                version: Some("16".to_string()),
                status: "running".to_string(),
                connection_info: None,
                created_at: String::new(),
                updated_at: String::new(),
                node_id: None,
                topology: "standalone".to_string(),
                members: vec![],
                error_message: None,
                metrics_enabled: false,
            },
            local_url: Some(local.to_string()),
            source_url: Some(source.to_string()),
        }
    }

    #[test]
    fn maps_plan_types_to_temps_service_types() {
        assert_eq!(
            ResourceExecutor::map_service_type("postgres"),
            Some(ServiceType::Postgres)
        );
        assert_eq!(
            ResourceExecutor::map_service_type("mysql"),
            Some(ServiceType::Mariadb)
        );
        assert_eq!(
            ResourceExecutor::map_service_type("redis"),
            Some(ServiceType::Redis)
        );
        assert_eq!(ResourceExecutor::map_service_type("clickhouse"), None);
    }

    #[test]
    fn dump_commands_exist_for_data_bearing_types() {
        assert!(dump_restore_command("postgres").is_some());
        assert!(dump_restore_command("mariadb").is_some());
        assert!(dump_restore_command("mongodb").is_some());
        assert!(dump_restore_command("redis").is_none());
    }

    #[test]
    fn rewrites_source_dsn_and_generated_domain() {
        let source_dsn = "postgres://postgres:pw@1.2.3.4:5432/postgres";
        let mut plan = plan_with(
            vec![
                ("DATABASE_URL", source_dsn),
                ("APP_URL", "http://lab-shop.1.2.3.4.sslip.io/checkout"),
                ("UNTOUCHED", "value"),
            ],
            "lab-shop.1.2.3.4.sslip.io",
        );
        let created = vec![record(source_dsn, "postgres://lab:new@localhost:15001/lab")];
        let rewrites = build_env_rewrites(&plan, &created, "lab.preview.temps.dev");

        let changed = apply_env_rewrites(&mut plan, &rewrites);
        assert_eq!(changed, 2);
        assert_eq!(
            plan.deployment.env_vars[0].value,
            "postgres://lab:new@localhost:15001/lab"
        );
        assert_eq!(
            plan.deployment.env_vars[1].value,
            "http://lab.preview.temps.dev/checkout"
        );
        assert_eq!(plan.deployment.env_vars[2].value, "value");
        assert!(plan.deployment.env_vars[0]
            .source_description
            .as_deref()
            .unwrap()
            .contains("rewritten"));
    }

    #[test]
    fn longest_rewrite_wins_over_embedded_hostname() {
        // The DSN contains the host 1.2.3.4 — the full-URL rewrite must be
        // applied before any shorter hostname rewrite could corrupt it.
        let source_dsn = "postgres://postgres:pw@db.1.2.3.4.sslip.io:5432/app";
        let mut plan = plan_with(vec![("DATABASE_URL", source_dsn)], "db.1.2.3.4.sslip.io");
        let created = vec![record(source_dsn, "postgres://new@localhost:15001/app")];
        let rewrites = build_env_rewrites(&plan, &created, "preview.temps.dev");
        apply_env_rewrites(&mut plan, &rewrites);
        assert_eq!(
            plan.deployment.env_vars[0].value,
            "postgres://new@localhost:15001/app"
        );
    }

    #[test]
    fn preview_hostname_composes_subdomain_and_domain() {
        let host = preview_hostname("preview.example.com", "lab");
        assert!(host.contains("lab"));
        assert!(host.ends_with("preview.example.com"));
    }
}

#[cfg(test)]
mod parameter_tests {
    use super::*;

    #[test]
    fn derives_database_and_username_from_source_url() {
        let params = service_parameters(
            &ServiceType::Postgres,
            "lab-db",
            Some("postgres://labuser:pw@1.2.3.4:5432/labdb"),
        );
        assert_eq!(params.get("database").unwrap(), "labdb");
        assert_eq!(params.get("username").unwrap(), "labuser");
    }

    #[test]
    fn falls_back_to_sanitized_name_without_source_url() {
        let params = service_parameters(&ServiceType::Postgres, "lab-db", None);
        assert_eq!(params.get("database").unwrap(), "lab_db");
        assert_eq!(params.get("username").unwrap(), "app");
    }

    #[test]
    fn redis_gets_only_a_port() {
        let params = service_parameters(&ServiceType::Redis, "cache", None);
        assert_eq!(params.len(), 1);
        assert!(
            params.contains_key("port"),
            "redis still needs a stable port"
        );
    }

    /// Regression test for the "unreachable source DB hangs a Tokio worker
    /// forever" bug: `wait_container` had no timeout, so a hung dump/restore
    /// container never returned control. Runs a container that sleeps far
    /// longer than the bound, confirms `wait_for_container` returns a timeout
    /// error (not hanging until the sleep finishes) and force-removes the
    /// container instead of leaking it. Skips gracefully if Docker isn't
    /// available, per this repo's Docker-test convention.
    #[tokio::test]
    async fn wait_for_container_times_out_on_a_hung_container() {
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, InspectContainerOptions, RemoveContainerOptions,
            StartContainerOptions,
        };

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker not available, skipping: {}", e);
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }

        // The fixture image must be pulled explicitly — CI runners start
        // clean, unlike a dev machine where busybox is often already cached
        // from other work, which is exactly why this failed in CI but not
        // locally the first time around.
        {
            use bollard::query_parameters::CreateImageOptions;
            use futures_util::StreamExt;
            let mut pull = docker.create_image(
                Some(CreateImageOptions {
                    from_image: Some("busybox:latest".to_string()),
                    ..Default::default()
                }),
                None,
                None,
            );
            while let Some(item) = pull.next().await {
                item.expect("pull busybox:latest fixture image");
            }
        }

        let name = format!(
            "temps-import-test-hang-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let container = docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some("busybox:latest".to_string()),
                    cmd: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "sleep 300".to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .expect("create hung-container fixture");
        docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .expect("start hung-container fixture");

        let started = std::time::Instant::now();
        let result = wait_for_container(&docker, &container.id, Duration::from_secs(2)).await;
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "a sleeping container must time out, not exit cleanly"
        );
        let message = result.unwrap_err();
        assert!(
            message.contains("timed out after 2s"),
            "error should name the bound that fired: {}",
            message
        );
        assert!(
            elapsed < Duration::from_secs(60),
            "must return promptly on timeout, not wait for the 300s sleep: took {:?}",
            elapsed
        );

        // The container must have been force-removed, not left running.
        let inspect = docker
            .inspect_container(&container.id, None::<InspectContainerOptions>)
            .await;
        assert!(
            inspect.is_err(),
            "timed-out container should have been force-removed, but it still exists"
        );

        // Best-effort cleanup in case the assertion above ever fails.
        let _ = docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }
}

#[cfg(test)]
mod annotation_tests {
    use super::*;

    #[test]
    fn skipped_domains_get_replacement_and_explanation() {
        let mut plan = tests::plan_with(vec![], "lab-shop.1.2.3.4.sslip.io");
        let annotated = annotate_skipped_domains(&mut plan, "lab.preview.temps.dev");
        assert_eq!(annotated, 1);
        let domain = &plan.domains[0];
        assert_eq!(domain.replacement.as_deref(), Some("lab.preview.temps.dev"));
        assert!(domain.action_description.contains("lab.preview.temps.dev"));
        assert!(domain.action_description.contains("source server's IP"));
    }
}
