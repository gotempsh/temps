// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins that run as Temps services.
//!
//! A plugin is the container of an ordinary deployment: Temps built it,
//! started it with the hardening every deployment gets, limits it, logs it
//! and can roll it back. This module never runs plugin code. It finds the
//! running container of the service a plugin is bound to, proves to it that
//! the caller is this Temps instance, and reads its manifest.
//!
//! Nothing is exchanged to make that work. Every deployment already carries a
//! deployment token; both sides derive the request-assertion secret from it
//! (see [`temps_plugin_protocol::SERVICE_AUTH_CONTEXT`]).

use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt as _;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder};
use sha2::{Digest as _, Sha256};
use temps_core::{ExecutionEnvironment, RuntimeContext, ServiceEndpointScheme};
use temps_entities::{deployment_containers, deployments, environments, nodes, projects};
use temps_plugin_protocol::{
    PluginServiceHello, EXTERNAL_PLUGIN_PROTOCOL_VERSION, PLUGIN_HELLO_PATH, SERVICE_AUTH_CONTEXT,
};
use thiserror::Error;

use crate::endpoint::PluginEndpoint;

/// How long a plugin service may take to answer its hello.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest hello Temps reads: a manifest and an OpenAPI document.
const MAX_HELLO_BYTES: usize = 4 * 1024 * 1024;

/// The Temps service a plugin runs as: one environment of one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceBinding {
    pub project_id: i32,
    pub environment_id: i32,
}

impl ServiceBinding {
    /// The identity the plugin's actor is bound to. Grants follow a plugin
    /// across deployments of the same environment and are never inherited by
    /// a different service that happens to use the same plugin name.
    pub fn source_identity(&self) -> String {
        format!("service:{}:{}", self.project_id, self.environment_id)
    }
}

/// Why Temps could not attach to the service a plugin runs as.
#[derive(Debug, Error)]
pub enum ServiceAttachError {
    #[error("Service {project_id} does not exist, so no plugin can run as it")]
    ServiceNotFound { project_id: i32 },
    #[error("Environment {environment_id} of service {project_id} does not exist, so no plugin can run in it")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Environment '{environment}' of service '{service}' is stopped because it was idle. It starts again on the next request to the plugin")]
    Asleep {
        service: String,
        environment: String,
    },
    #[error("Environment '{environment}' of service '{service}' has no deployment yet. Deploy the service, then add it as a plugin")]
    NotDeployed {
        service: String,
        environment: String,
    },
    #[error("Deployment {deployment_id} of service '{service}' has no running container that Temps can reach. Check the deployment's logs")]
    NoReachableContainer { service: String, deployment_id: i32 },
    #[error("Temps could not read the deployment token of service '{service}', which it authenticates to the plugin with: {reason}")]
    Token { service: String, reason: String },
    #[error("Service '{service}' did not answer at {endpoint}: {reason}")]
    Unreachable {
        service: String,
        endpoint: String,
        reason: String,
    },
    #[error("Service '{service}' refused this Temps instance. A plugin derives its secret from the deployment token it was started with; redeploy the service so it picks up the current one")]
    Refused { service: String },
    #[error("Service '{service}' is not a plugin: it does not answer {path}. Build it with the Temps plugin SDK, which serves that path when the plugin runs as a service", path = PLUGIN_HELLO_PATH)]
    NotAPlugin { service: String },
    #[error("Service '{service}' answered the plugin hello with HTTP {status}")]
    UnexpectedStatus { service: String, status: u16 },
    #[error("Service '{service}' sent a plugin hello Temps cannot read: {reason}")]
    InvalidHello { service: String, reason: String },
    #[error("Service '{service}' speaks plugin protocol {found}, but this Temps build requires {required}; rebuild the plugin with the matching plugin SDK", required = EXTERNAL_PLUGIN_PROTOCOL_VERSION)]
    ProtocolMismatch { service: String, found: u32 },
    #[error("Service '{service}' declares the plugin name '{name}', which is not a valid plugin name: lowercase letters, digits and hyphens, at most 64 characters")]
    InvalidName { service: String, name: String },
    #[error("Service '{service}' now declares the plugin '{found}', but it was added as the plugin '{expected}'. Remove the plugin and add the service again to accept the new name")]
    NameChanged {
        service: String,
        expected: String,
        found: String,
    },
    #[error("Plugin '{plugin}' asks for {access}, which a community plugin is never given. Remove that request from its manifest; link a database to the service for the plugin's own data")]
    PrivilegedAccess {
        plugin: String,
        access: &'static str,
    },
    #[error("Service '{service}' now runs deployment {deployment_id}, which is not the build of plugin '{plugin}' an administrator approved. The plugin stays disconnected until the update is approved in Settings > Plugins, or the service runs the approved build again")]
    UpdateWaiting {
        plugin: String,
        service: String,
        deployment_id: i32,
    },
    #[error("Plugin '{plugin}' was disabled by an administrator, so Temps is not connected to it. Enable it in Settings > Plugins to connect it again")]
    Disabled { plugin: String },
    #[error("Plugin '{plugin}' did not accept the platform channel from this Temps instance")]
    ChannelRefused { plugin: String },
    #[error("A plugin named '{name}' is already installed from another source. Remove it before adding a service that declares the same plugin name")]
    NameTaken { name: String },
    #[error("Plugin '{plugin}' is attached, but Temps could not record it: {reason}")]
    Record { plugin: String, reason: String },
    #[error(
        "This Temps process cannot run plugins as services: it has no deployments to attach to"
    )]
    Unavailable,
    #[error("Database error while resolving the service a plugin runs as: {0}")]
    Database(#[from] DbErr),
}

impl ServiceAttachError {
    /// Whether the plugin is merely stopped for being idle, which the next
    /// request fixes, as opposed to broken.
    pub fn is_asleep(&self) -> bool {
        matches!(self, Self::Asleep { .. })
    }

    /// Whether the service runs a build no administrator approved, which
    /// only an approval or the approved build running again changes.
    pub fn is_update_waiting(&self) -> bool {
        matches!(self, Self::UpdateWaiting { .. })
    }

    /// What an administrator is shown. Every variant already names the
    /// service and says what to do, and none carries a secret or a host path:
    /// endpoints are `host:port` of a container Temps itself deployed.
    pub fn public_reason(&self) -> String {
        match self {
            Self::Database(_) => {
                "Temps could not read the plugin's service from its database. See the server log."
                    .to_string()
            }
            other => other.to_string(),
        }
    }
}

pub use temps_core::DeploymentTokenSource;

/// The secret Temps presents to a plugin service: the lowercase hex SHA-256
/// of [`SERVICE_AUTH_CONTEXT`], a NUL byte and the service's deployment
/// token. The plugin SDK computes the same value.
pub fn service_auth_secret(api_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SERVICE_AUTH_CONTEXT.as_bytes());
    hasher.update([0u8]);
    hasher.update(api_token.as_bytes());
    hex::encode(hasher.finalize())
}

/// The running container of a plugin's service, as the database describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceTarget {
    pub binding: ServiceBinding,
    /// The service's name, for messages.
    pub service: String,
    pub deployment_id: i32,
    /// Identifies the exact build that is running. It changes with every
    /// deployment, so consent given to one build is not inherited by the next.
    pub build: String,
    pub endpoint: PluginEndpoint,
}

/// Find the container Temps should talk to for `binding`.
pub(crate) async fn resolve_target(
    db: &DatabaseConnection,
    runtime: &RuntimeContext,
    binding: ServiceBinding,
) -> Result<ServiceTarget, ServiceAttachError> {
    let project = projects::Entity::find_by_id(binding.project_id)
        .filter(projects::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .ok_or(ServiceAttachError::ServiceNotFound {
            project_id: binding.project_id,
        })?;
    let environment = environments::Entity::find_by_id(binding.environment_id)
        .filter(environments::Column::ProjectId.eq(binding.project_id))
        .filter(environments::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .ok_or(ServiceAttachError::EnvironmentNotFound {
            project_id: binding.project_id,
            environment_id: binding.environment_id,
        })?;
    if environment.sleeping {
        return Err(ServiceAttachError::Asleep {
            service: project.name,
            environment: environment.name,
        });
    }
    let Some(deployment_id) = environment.current_deployment_id else {
        return Err(ServiceAttachError::NotDeployed {
            service: project.name,
            environment: environment.name,
        });
    };
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Err(ServiceAttachError::NotDeployed {
            service: project.name,
            environment: environment.name,
        });
    };
    // A plugin has one channel, so Temps attaches to one container: the
    // deployment's first. Run a plugin service with a single replica.
    let containers = deployment_containers::Entity::find()
        .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_containers::Column::DeletedAt.is_null())
        .order_by_asc(deployment_containers::Column::Id)
        .all(db)
        .await?;
    let mut endpoint = None;
    for container in &containers {
        let node_address = match container.node_id {
            None => None,
            Some(node_id) => match nodes::Entity::find_by_id(node_id).one(db).await? {
                Some(node) => Some(node.data_address().to_string()),
                // The node is gone; so is this container.
                None => continue,
            },
        };
        if let Some(authority) = container_authority(container, node_address.as_deref(), runtime) {
            endpoint = Some(PluginEndpoint::Tcp(authority));
            break;
        }
    }
    let Some(endpoint) = endpoint else {
        return Err(ServiceAttachError::NoReachableContainer {
            service: project.name,
            deployment_id,
        });
    };
    Ok(ServiceTarget {
        binding,
        service: project.name,
        deployment_id,
        build: build_identity(binding, &deployment),
        endpoint,
    })
}

/// The address a deployment container answers on, by the same rules the
/// route table uses: a container on another node is reached on that node's
/// data address and published port; a local one by its published port when
/// Temps runs on the host, or by name on the Docker network when Temps runs
/// in a container. A container Temps cannot reach has no address.
fn container_authority(
    container: &deployment_containers::Model,
    node_address: Option<&str>,
    runtime: &RuntimeContext,
) -> Option<String> {
    let container_port = u16::try_from(container.container_port?).ok()?;
    let host_port = match container.host_port {
        Some(port) => Some(u16::try_from(port).ok()?),
        None => None,
    };
    if let Some(node_address) = node_address {
        let port = host_port?;
        return Some(match node_address.parse::<std::net::IpAddr>() {
            Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
            Err(_) => format!("{node_address}:{port}"),
        });
    }
    if runtime.execution_environment() == ExecutionEnvironment::Host && host_port.is_none() {
        return None;
    }
    Some(
        runtime
            .resolve_service_endpoint(
                &container.container_name,
                ServiceEndpointScheme::Http,
                container_port,
                host_port.unwrap_or(container_port),
            )
            .authority(),
    )
}

/// A stable name for one build of a plugin service.
fn build_identity(binding: ServiceBinding, deployment: &deployments::Model) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"temps-plugin-service-build-v1\0");
    hasher.update(binding.project_id.to_be_bytes());
    hasher.update(binding.environment_id.to_be_bytes());
    hasher.update(deployment.id.to_be_bytes());
    hasher.update(
        deployment
            .image_name
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update([0u8]);
    hasher.update(
        deployment
            .commit_sha
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hex::encode(hasher.finalize())
}

/// Ask a plugin service for its manifest, proving who is asking.
pub(crate) async fn hello(
    service: &str,
    endpoint: &PluginEndpoint,
    auth_secret: &str,
) -> Result<PluginServiceHello, ServiceAttachError> {
    let unreachable = |reason: String| ServiceAttachError::Unreachable {
        service: service.to_string(),
        endpoint: endpoint.host_header().to_string(),
        reason,
    };
    let exchange = async {
        let stream = endpoint
            .connect()
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|error| unreachable(error.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(PLUGIN_HELLO_PATH)
            .header(hyper::header::HOST, endpoint.host_header())
            .header(temps_plugin_protocol::headers::AUTH_SIGNATURE, auth_secret)
            .body(http_body_util::Empty::<axum::body::Bytes>::new())
            .map_err(|error| unreachable(error.to_string()))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        let status = response.status();
        match status.as_u16() {
            200 => {}
            401 | 403 => {
                return Err(ServiceAttachError::Refused {
                    service: service.to_string(),
                })
            }
            404 | 405 => {
                return Err(ServiceAttachError::NotAPlugin {
                    service: service.to_string(),
                })
            }
            other => {
                return Err(ServiceAttachError::UnexpectedStatus {
                    service: service.to_string(),
                    status: other,
                })
            }
        }
        let body = http_body_util::Limited::new(response.into_body(), MAX_HELLO_BYTES)
            .collect()
            .await
            .map_err(|error| ServiceAttachError::InvalidHello {
                service: service.to_string(),
                reason: format!(
                    "the answer could not be read within {MAX_HELLO_BYTES} bytes: {error}"
                ),
            })?
            .to_bytes();
        serde_json::from_slice::<PluginServiceHello>(&body).map_err(|error| {
            ServiceAttachError::InvalidHello {
                service: service.to_string(),
                reason: error.to_string(),
            }
        })
    };
    let hello = tokio::time::timeout(HELLO_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            unreachable(format!(
                "no answer within {} seconds",
                HELLO_TIMEOUT.as_secs()
            ))
        })??;
    check_hello(service, &hello)?;
    Ok(hello)
}

/// What a hello must satisfy before Temps opens a channel to the plugin.
fn check_hello(service: &str, hello: &PluginServiceHello) -> Result<(), ServiceAttachError> {
    if hello.protocol_version != EXTERNAL_PLUGIN_PROTOCOL_VERSION {
        return Err(ServiceAttachError::ProtocolMismatch {
            service: service.to_string(),
            found: hello.protocol_version,
        });
    }
    if !crate::naming::valid_plugin_name(&hello.manifest.name) {
        return Err(ServiceAttachError::InvalidName {
            service: service.to_string(),
            name: hello.manifest.name.clone(),
        });
    }
    if let Some(access) = crate::manager::privileged_access(&hello.manifest) {
        return Err(ServiceAttachError::PrivilegedAccess {
            plugin: hello.manifest.name.clone(),
            access,
        });
    }
    Ok(())
}

/// What the plugin host needs from the rest of the platform to attach to
/// services. Absent on a server that has no deployments.
#[derive(Clone)]
pub struct ServiceRuntime {
    pub(crate) tokens: Arc<dyn DeploymentTokenSource>,
    pub(crate) runtime: RuntimeContext,
    /// Starts a stopped environment again and keeps a used one awake. `None`
    /// when this process does not run the proxy that owns that lifecycle.
    pub(crate) waker: Option<Arc<dyn temps_core::OnDemandWaker>>,
}

impl ServiceRuntime {
    pub fn new(tokens: Arc<dyn DeploymentTokenSource>, runtime: RuntimeContext) -> Self {
        Self {
            tokens,
            runtime,
            waker: None,
        }
    }

    pub fn with_waker(mut self, waker: Arc<dyn temps_core::OnDemandWaker>) -> Self {
        self.waker = Some(waker);
        self
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use async_trait::async_trait;

    /// A token source that always returns the same token.
    pub(crate) struct FixedToken(pub &'static str);

    #[async_trait]
    impl DeploymentTokenSource for FixedToken {
        async fn deployment_token(&self, _: i32, _: i32) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    /// A token source that cannot read any token.
    pub(crate) struct NoToken;

    #[async_trait]
    impl DeploymentTokenSource for NoToken {
        async fn deployment_token(&self, _: i32, _: i32) -> Result<String, String> {
            Err("the token store is unavailable".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use temps_plugin_protocol::PluginManifest;

    fn container(
        container_port: Option<i32>,
        host_port: Option<i32>,
        node_id: Option<i32>,
    ) -> deployment_containers::Model {
        deployment_containers::Model {
            id: 1,
            deployment_id: 7,
            container_id: "c0ffee".to_string(),
            container_name: "reports-production-7".to_string(),
            container_port,
            health_state: "healthy".to_string(),
            health_checked_at: None,
            health_detail: None,
            host_port,
            image_name: None,
            status: Some("running".to_string()),
            service_name: None,
            created_at: chrono::Utc::now(),
            deployed_at: chrono::Utc::now(),
            ready_at: None,
            deleted_at: None,
            node_id,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
            port_bindings: None,
        }
    }

    #[test]
    fn the_secret_matches_what_the_plugin_sdk_derives() {
        // The same vector is asserted in temps-plugin-sdk: the two sides must
        // agree without ever exchanging the value.
        assert_eq!(
            service_auth_secret("dt_example"),
            "2ec7a454828e34191fc092dbafcb99df2d280ae2dac66df185cfe83666092593"
        );
        assert_ne!(
            service_auth_secret("dt_example"),
            service_auth_secret("dt_other")
        );
    }

    #[test]
    fn a_binding_names_one_environment_of_one_service() {
        let binding = ServiceBinding {
            project_id: 12,
            environment_id: 34,
        };
        assert_eq!(binding.source_identity(), "service:12:34");
    }

    #[test]
    fn a_local_container_is_reached_on_its_published_port_from_the_host() {
        let address = container_authority(
            &container(Some(8080), Some(49160), None),
            None,
            &RuntimeContext::host(),
        );
        assert_eq!(address.as_deref(), Some("127.0.0.1:49160"));
    }

    #[test]
    fn a_local_container_without_a_published_port_cannot_be_reached_from_the_host() {
        // Guessing 127.0.0.1:<container port> would dial whatever else owns
        // that port on the host and send it the plugin's secret.
        assert_eq!(
            container_authority(
                &container(Some(8080), None, None),
                None,
                &RuntimeContext::host()
            ),
            None
        );
    }

    #[test]
    fn a_local_container_is_reached_by_name_when_temps_runs_in_docker() {
        let address = container_authority(
            &container(Some(8080), Some(49160), None),
            None,
            &RuntimeContext::docker(),
        );
        assert_eq!(address.as_deref(), Some("reports-production-7:8080"));
    }

    #[test]
    fn a_container_on_another_node_is_reached_on_that_nodes_published_port() {
        let remote = container(Some(8080), Some(49160), Some(3));
        assert_eq!(
            container_authority(&remote, Some("10.20.0.7"), &RuntimeContext::docker()).as_deref(),
            Some("10.20.0.7:49160")
        );
        assert_eq!(
            container_authority(&remote, Some("fd00::7"), &RuntimeContext::host()).as_deref(),
            Some("[fd00::7]:49160")
        );
        assert_eq!(
            container_authority(
                &container(Some(8080), None, Some(3)),
                Some("10.20.0.7"),
                &RuntimeContext::host()
            ),
            None,
            "a remote container is only reachable through a published port"
        );
    }

    #[test]
    fn a_container_without_a_port_has_no_address() {
        assert_eq!(
            container_authority(
                &container(None, Some(49160), None),
                None,
                &RuntimeContext::host()
            ),
            None
        );
    }

    const SECRET: &str = "the-derived-secret";

    /// A fake plugin service answering the hello path with `answer`.
    async fn hello_server<F, R>(answer: F) -> PluginEndpoint
    where
        F: Fn(HeaderMap) -> R + Clone + Send + Sync + 'static,
        R: IntoResponse + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            PLUGIN_HELLO_PATH,
            get(move |headers: HeaderMap| {
                let answer = answer.clone();
                async move { answer(headers) }
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        PluginEndpoint::Tcp(address.to_string())
    }

    fn hello_body(manifest: PluginManifest, protocol_version: u32) -> String {
        serde_json::to_string(&PluginServiceHello {
            protocol_version,
            manifest: Box::new(manifest),
            has_ui: false,
            openapi: None,
        })
        .unwrap()
    }

    fn authenticated(headers: &HeaderMap) -> bool {
        headers
            .get(temps_plugin_protocol::headers::AUTH_SIGNATURE)
            .and_then(|value| value.to_str().ok())
            == Some(SECRET)
    }

    #[tokio::test]
    async fn a_plugin_service_answers_its_manifest_to_the_secret() {
        let endpoint = hello_server(|headers: HeaderMap| {
            if !authenticated(&headers) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            hello_body(
                PluginManifest::builder("reports", "2.0.0").build(),
                EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            )
            .into_response()
        })
        .await;

        let hello = hello("Reports", &endpoint, SECRET)
            .await
            .expect("the plugin answers the platform");
        assert_eq!(hello.manifest.name, "reports");
        assert_eq!(hello.manifest.version, "2.0.0");

        let refused = super::hello("Reports", &endpoint, "another-secret")
            .await
            .expect_err("a different token gives a different secret");
        assert!(matches!(refused, ServiceAttachError::Refused { .. }));
        assert!(refused.to_string().contains("redeploy the service"));
    }

    #[tokio::test]
    async fn an_ordinary_service_is_not_a_plugin() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = PluginEndpoint::Tcp(listener.local_addr().unwrap().to_string());
        let app = axum::Router::new().route("/", get(|| async { "an ordinary app" }));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let error = hello("Storefront", &endpoint, SECRET)
            .await
            .expect_err("the service has no plugin hello");
        assert!(matches!(error, ServiceAttachError::NotAPlugin { .. }));
        assert_eq!(
            error.to_string(),
            "Service 'Storefront' is not a plugin: it does not answer /_temps/plugin/hello. Build it with the Temps plugin SDK, which serves that path when the plugin runs as a service"
        );
    }

    #[tokio::test]
    async fn a_service_that_is_down_is_named_with_its_address() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);

        let error = hello("Reports", &PluginEndpoint::Tcp(address.clone()), SECRET)
            .await
            .expect_err("nothing listens there");
        match error {
            ServiceAttachError::Unreachable {
                service, endpoint, ..
            } => {
                assert_eq!(service, "Reports");
                assert_eq!(endpoint, address);
            }
            other => panic!("expected an unreachable service, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_hello_is_refused_for_the_wrong_protocol_a_bad_name_or_privileged_access() {
        let endpoint = hello_server(|_| {
            hello_body(PluginManifest::builder("reports", "1.0.0").build(), 1).into_response()
        })
        .await;
        let error = hello("Reports", &endpoint, SECRET).await.unwrap_err();
        assert!(matches!(
            error,
            ServiceAttachError::ProtocolMismatch { found: 1, .. }
        ));

        let endpoint = hello_server(|_| {
            hello_body(
                PluginManifest::builder("../etc", "1.0.0").build(),
                EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            )
            .into_response()
        })
        .await;
        let error = hello("Reports", &endpoint, SECRET).await.unwrap_err();
        assert!(matches!(error, ServiceAttachError::InvalidName { .. }));

        let endpoint = hello_server(|_| {
            hello_body(
                PluginManifest::builder("reports", "1.0.0")
                    .requires_db(true)
                    .build(),
                EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            )
            .into_response()
        })
        .await;
        let error = hello("Reports", &endpoint, SECRET).await.unwrap_err();
        assert!(matches!(error, ServiceAttachError::PrivilegedAccess { .. }));
        assert!(error
            .to_string()
            .contains("link a database to the service for the plugin's own data"));

        let endpoint = hello_server(|_| "not json".into_response()).await;
        let error = hello("Reports", &endpoint, SECRET).await.unwrap_err();
        assert!(matches!(error, ServiceAttachError::InvalidHello { .. }));

        let endpoint = hello_server(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;
        let error = hello("Reports", &endpoint, SECRET).await.unwrap_err();
        assert!(matches!(
            error,
            ServiceAttachError::UnexpectedStatus { status: 500, .. }
        ));
    }

    #[test]
    fn only_a_stopped_environment_counts_as_asleep() {
        let asleep = ServiceAttachError::Asleep {
            service: "Reports".to_string(),
            environment: "production".to_string(),
        };
        assert!(asleep.is_asleep());
        assert!(!ServiceAttachError::ServiceNotFound { project_id: 4 }.is_asleep());
        assert_eq!(
            ServiceAttachError::ServiceNotFound { project_id: 4 }.public_reason(),
            "Service 4 does not exist, so no plugin can run as it"
        );
        let database = ServiceAttachError::Database(DbErr::Custom("relation missing".to_string()));
        assert!(!database.public_reason().contains("relation missing"));
    }
}
