// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A plugin built with the plugin SDK, served on a local port the way a
//! deployment's container serves it, and the plugin host attaching to it
//! through a real database.

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::temps_core::RuntimeContext;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, Set, Statement,
};
use temps_plugin_sdk::prelude::{ExternalPlugin, PluginContext, PluginManifest};
use temps_plugin_sdk::runtime::{serve_service, ServiceEnvironment};
use tower::ServiceExt;

use super::*;
use crate::manager::ExternalPluginConfig;
use crate::service_runtime::testing::{FixedToken, NoToken};
use temps_plugin_protocol::channel::PluginHostPermission;

/// The deployment token of every fixture service.
pub(crate) const TOKEN: &str = "dt_fixture";

/// A plugin with one route, named by the test. It subscribes to deployment
/// events and counts the ones it is given, asks to change platform data and
/// serves one path without a Temps session.
struct Reports {
    name: &'static str,
    privileged: bool,
    /// Permissions a later build asks for on top of the usual ones.
    extra: Vec<PluginHostPermission>,
    events: Arc<AtomicUsize>,
}

impl ExternalPlugin for Reports {
    fn manifest(&self) -> PluginManifest {
        let mut manifest = PluginManifest::builder(self.name, "1.0.0")
            .display_name("Reports")
            .host_permission(PluginHostPermission::EventsRead)
            .host_permission(PluginHostPermission::ApiWrite)
            .public_path("/hooks/*")
            .event("deployment.succeeded")
            .build();
        manifest.host_permissions.extend(self.extra.iter().copied());
        manifest.requires_db = self.privileged;
        manifest
    }

    fn router(&self, _ctx: PluginContext) -> axum::Router {
        axum::Router::new().route("/items", axum::routing::get(|| async { "three items" }))
    }

    fn on_event(&self, _ctx: &PluginContext, _event: temps_plugin_protocol::PluginEvent) {
        self.events.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

/// A plugin service listening on a local port, in a runtime of its own:
/// stopping it closes every connection, as stopping a container does.
pub(crate) struct Served {
    pub(crate) port: u16,
    /// How many events the plugin was given.
    pub(crate) events: Arc<AtomicUsize>,
    runtime: Option<tokio::runtime::Runtime>,
    _data: tempfile::TempDir,
}

impl Served {
    pub(crate) fn stop(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

pub(crate) fn serve_plugin(name: &'static str, token: &'static str) -> Served {
    serve_plugin_asking(name, token, Vec::new())
}

/// A build of the plugin that asks for `extra` permissions as well.
pub(crate) fn serve_plugin_asking(
    name: &'static str,
    token: &'static str,
    extra: Vec<PluginHostPermission>,
) -> Served {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free local port");
    listener.set_nonblocking(true).expect("non-blocking");
    let port = listener.local_addr().expect("local address").port();
    let data = tempfile::tempdir().expect("plugin data directory");
    let data_dir = data.path().to_string_lossy().into_owned();
    let environment = ServiceEnvironment::from_lookup(|variable| match variable {
        "TEMPS_API_TOKEN" => Some(token.to_string()),
        "TEMPS_PLUGIN_DATA_DIR" => Some(data_dir.clone()),
        _ => None,
    })
    .expect("service environment");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("plugin runtime");
    let events = Arc::new(AtomicUsize::new(0));
    let counted = events.clone();
    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
        let plugin = Reports {
            name,
            privileged: false,
            extra,
            events: counted,
        };
        let _ = serve_service(plugin, listener, environment).await;
    });
    Served {
        port,
        events,
        runtime: Some(runtime),
        _data: data,
    }
}

/// An ordinary web service: it answers, but it is not a plugin.
async fn serve_ordinary_service() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("a free local port");
    let port = listener.local_addr().expect("local address").port();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "hello" }));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, task)
}

/// Starts a stopped environment by running the SQL a test gave it, the way
/// the on-demand manager starts containers and clears `sleeping`.
#[derive(Default)]
pub(crate) struct FakeWaker {
    db: Mutex<Option<Arc<DatabaseConnection>>>,
    on_wake: Mutex<Vec<String>>,
    pub(crate) wakes: AtomicUsize,
    pub(crate) activity: AtomicUsize,
    fail: std::sync::atomic::AtomicBool,
}

impl FakeWaker {
    fn starts_by(&self, statements: Vec<String>) {
        *self.on_wake.lock().expect("waker lock") = statements;
    }
}

#[async_trait]
impl ::temps_core::OnDemandWaker for FakeWaker {
    async fn wake_environment(
        &self,
        _environment_id: i32,
        _wake_timeout_seconds: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.wakes.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail.load(AtomicOrdering::SeqCst) {
            return Err("the container did not become healthy".into());
        }
        let db = self.db.lock().expect("waker lock").clone();
        let statements = self.on_wake.lock().expect("waker lock").clone();
        if let Some(db) = db {
            for statement in statements {
                db.execute_unprepared(&statement).await?;
            }
        }
        Ok(())
    }

    async fn sleep_environment(
        &self,
        _environment_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(false)
    }

    fn record_activity(&self, _environment_id: i32) {
        self.activity.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

/// A Temps instance: a database and a plugin manager that can attach to
/// services on this machine.
pub(crate) struct Platform {
    pub(crate) database: temps_test_utils::TestDatabase,
    pub(crate) root: tempfile::TempDir,
    pub(crate) waker: Arc<FakeWaker>,
}

pub(crate) async fn platform(reason: &str) -> Option<Platform> {
    let database = match temps_test_utils::TestDatabase::with_migrations().await {
        Ok(database) => database,
        Err(error) if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) => {
            eprintln!("Skipping {reason}: Docker unavailable");
            return None;
        }
        Err(error) => panic!("fixture database: {error}"),
    };
    let waker = Arc::new(FakeWaker::default());
    *waker.db.lock().expect("waker lock") = Some(database.connection_arc());
    Some(Platform {
        database,
        root: tempfile::tempdir().expect("plugin host directory"),
        waker,
    })
}

impl Platform {
    pub(crate) fn db(&self) -> Arc<DatabaseConnection> {
        self.database.connection_arc()
    }

    pub(crate) fn config(&self) -> ExternalPluginConfig {
        ExternalPluginConfig::new(self.root.path().into())
    }

    pub(crate) fn runtime(&self) -> ServiceRuntime {
        ServiceRuntime::new(Arc::new(FixedToken(TOKEN)), RuntimeContext::host())
            .with_waker(self.waker.clone())
    }

    /// A manager as a freshly started Temps process has it.
    async fn manager(&self) -> Arc<ExternalPluginManager> {
        let manager = ExternalPluginManager::new(self.config(), self.db()).shared();
        manager.set_service_runtime(self.runtime()).await;
        manager
    }

    /// A service with one environment and nothing deployed.
    pub(crate) async fn service(&self, name: &str) -> ServiceBinding {
        let now = chrono::Utc::now();
        let project = temps_entities::projects::ActiveModel {
            name: Set(name.to_string()),
            slug: Set(format!("{name}-{}", uuid::Uuid::new_v4().simple())),
            repo_name: Set("plugin".into()),
            repo_owner: Set("example".into()),
            directory: Set("/".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db().as_ref())
        .await
        .expect("service");
        let environment = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(format!("plugin-{}", project.id)),
            host: Set(format!("plugin-{}.example.test", project.id)),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            ..Default::default()
        }
        .insert(self.db().as_ref())
        .await
        .expect("environment");
        ServiceBinding {
            project_id: project.id,
            environment_id: environment.id,
        }
    }

    /// Deploy `binding`: a new deployment whose one container publishes
    /// `port` on this machine becomes the environment's current deployment.
    pub(crate) async fn deploy(&self, binding: ServiceBinding, port: u16) -> i32 {
        let db = self.db();
        let deployment_id: i32 = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO deployments (project_id, environment_id, slug, state, metadata, \
                     commit_sha, commit_message, ready_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, 'deployed', '{}'::jsonb, $4, 'Synthetic change', NOW(), \
                     NOW(), NOW()) RETURNING id",
                [
                    binding.project_id.into(),
                    binding.environment_id.into(),
                    format!("deploy-{}", uuid::Uuid::new_v4().simple()).into(),
                    format!("{:040x}", uuid::Uuid::new_v4().as_u128()).into(),
                ],
            ))
            .await
            .expect("deployment")
            .expect("deployment row")
            .try_get("", "id")
            .expect("deployment id");
        let now = chrono::Utc::now();
        temps_entities::deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            container_id: Set(format!("{:032x}", uuid::Uuid::new_v4().as_u128())),
            container_name: Set(format!("plugin-{deployment_id}")),
            container_port: Set(Some(8080)),
            host_port: Set(Some(i32::from(port))),
            health_state: Set("healthy".to_string()),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("deployment container");
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE environments SET current_deployment_id=$1, sleeping=FALSE WHERE id=$2",
            [deployment_id.into(), binding.environment_id.into()],
        ))
        .await
        .expect("current deployment");
        deployment_id
    }

    /// Deploy `image` to the service the way an image deployment records
    /// it: no commit, and the image reference the client named.
    pub(crate) async fn deploy_image(
        &self,
        binding: ServiceBinding,
        port: u16,
        image: &str,
    ) -> i32 {
        let deployment_id = self.deploy(binding, port).await;
        self.db()
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE deployments SET commit_sha=NULL, commit_message=NULL, image_name=$2, \
                     metadata=jsonb_build_object('externalImageRef', $2::text) WHERE id=$1",
                [deployment_id.into(), image.into()],
            ))
            .await
            .expect("image deployment");
        deployment_id
    }

    /// A user with this id, for what records who did something.
    pub(crate) async fn user(&self, id: i32) -> i32 {
        use sea_orm::IntoActiveModel;
        let user = crate::proxy::tests::test_user(id, &format!("operator-{id}@example.test"));
        temps_entities::users::Entity::insert(user.into_active_model())
            .exec(self.db().as_ref())
            .await
            .expect("user");
        id
    }

    /// Record the limits a deployment's container was started with.
    pub(crate) async fn limit(&self, deployment_id: i32, memory_mb: i32, cpu_microcores: i32) {
        self.db()
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE deployments SET deployment_config=\
                     jsonb_build_object('memoryLimit', $2::int, 'cpuLimit', $3::int) WHERE id=$1",
                [
                    deployment_id.into(),
                    memory_mb.into(),
                    cpu_microcores.into(),
                ],
            ))
            .await
            .expect("deployment limits");
    }

    pub(crate) async fn set_sleeping(&self, binding: ServiceBinding, sleeping: bool) {
        self.db()
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE environments SET sleeping=$1 WHERE id=$2",
                [sleeping.into(), binding.environment_id.into()],
            ))
            .await
            .expect("sleeping flag");
    }

    /// The stored plugin row: isolation, service, environment, whether a
    /// manifest is kept, and whether it is active.
    pub(crate) async fn stored(
        &self,
        name: &str,
    ) -> Option<(String, Option<i32>, Option<i32>, bool, bool)> {
        let row = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT isolation, project_id, environment_id, manifest IS NOT NULL AS kept, \
                     active FROM external_plugin_actors WHERE plugin_name=$1 \
                 ORDER BY active DESC, updated_at DESC LIMIT 1",
                [name.into()],
            ))
            .await
            .expect("plugin row")?;
        Some((
            row.try_get("", "isolation").expect("isolation"),
            row.try_get("", "project_id").expect("project"),
            row.try_get("", "environment_id").expect("environment"),
            row.try_get("", "kept").expect("manifest"),
            row.try_get("", "active").expect("active"),
        ))
    }
}

/// Poll `condition` for up to ten seconds.
pub(crate) async fn eventually<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if condition().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting until {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn status_of(manager: &ExternalPluginManager, name: &str) -> Option<ServicePluginStatus> {
    manager
        .service_plugin_statuses()
        .await
        .into_iter()
        .find(|status| status.name == name)
}

/// One request to a plugin through the proxy route Temps mounts for it.
pub(crate) async fn through_proxy(
    manager: &ExternalPluginManager,
    name: &str,
    path: &str,
    signed_in: bool,
) -> (StatusCode, String) {
    let proxy = manager
        .proxy_for(name)
        .await
        .expect("the plugin has a route");
    let router = crate::proxy::create_plugin_proxy_router(proxy);
    let mut request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request");
    if signed_in {
        request
            .extensions_mut()
            .insert(temps_auth::AuthContext::new_session(
                crate::proxy::tests::test_user(42, "operator@example.test"),
                temps_auth::permissions::Role::Admin,
            ));
    }
    let response = router.oneshot(request).await.expect("proxy response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("response body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deployed_service_becomes_a_plugin_and_answers_through_the_proxy() {
    let Some(platform) = platform("attaching to a plugin service").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let deployment_id = platform.deploy(binding, plugin.port).await;
    let manager = platform.manager().await;

    let manifest = manager
        .attach_service(binding, None)
        .await
        .expect("a deployed plugin service attaches");

    assert_eq!(manifest.name, "reports");
    assert_eq!(
        platform.stored("reports").await,
        Some((
            "service".to_string(),
            Some(binding.project_id),
            Some(binding.environment_id),
            true,
            true
        )),
        "the row names the service, keeps the manifest and is active"
    );
    assert_eq!(
        status_of(&manager, "reports").await,
        Some(ServicePluginStatus {
            name: "reports".to_string(),
            binding: Some(binding),
            deployment_id: Some(deployment_id),
            state: ServicePluginState::Running,
            detail: None,
        })
    );
    assert!(manager.is_service_plugin("reports").await);
    assert_eq!(manager.service_binding_of("reports").await, Some(binding));

    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));
    assert_eq!(
        platform.waker.activity.load(AtomicOrdering::SeqCst),
        1,
        "a request through Temps counts as use of the service"
    );

    // The proxy still authenticates: an anonymous caller never reaches the
    // service, and never starts or keeps it awake.
    let (status, _) = through_proxy(&manager, "reports", "/items", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(platform.waker.activity.load(AtomicOrdering::SeqCst), 1);

    // The hello is platform-only, like the channel.
    let (status, _) = through_proxy(
        &manager,
        "reports",
        temps_plugin_protocol::PLUGIN_HELLO_PATH,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_that_cannot_be_a_plugin_is_refused_with_the_reason_and_nothing_is_recorded() {
    let Some(platform) = platform("refusing services that are not plugins").await else {
        return;
    };
    let manager = platform.manager().await;

    // No such service: the error carries the id that was asked for.
    let missing = ServiceBinding {
        project_id: 987_654,
        environment_id: 1,
    };
    let error = manager.attach_service(missing, None).await.unwrap_err();
    assert!(
        matches!(
            error,
            ServiceAttachError::ServiceNotFound {
                project_id: 987_654
            }
        ),
        "{error}"
    );

    // An environment of another service.
    let binding = platform.service("reports-service").await;
    let other = platform.service("other-service").await;
    let crossed = ServiceBinding {
        project_id: binding.project_id,
        environment_id: other.environment_id,
    };
    let error = manager.attach_service(crossed, None).await.unwrap_err();
    assert!(
        matches!(error, ServiceAttachError::EnvironmentNotFound { .. }),
        "{error}"
    );

    // Nothing deployed yet.
    let error = manager.attach_service(binding, None).await.unwrap_err();
    assert!(
        matches!(error, ServiceAttachError::NotDeployed { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("Deploy the service"), "{error}");

    // A service that answers but is not a plugin.
    let (port, ordinary) = serve_ordinary_service().await;
    platform.deploy(binding, port).await;
    let error = manager.attach_service(binding, None).await.unwrap_err();
    assert!(
        matches!(error, ServiceAttachError::NotAPlugin { .. }),
        "{error}"
    );
    ordinary.abort();

    // A plugin started with another deployment token refuses this instance.
    let stranger = serve_plugin("reports", "dt_another_instance");
    platform.deploy(binding, stranger.port).await;
    let error = manager.attach_service(binding, None).await.unwrap_err();
    assert!(
        matches!(error, ServiceAttachError::Refused { .. }),
        "{error}"
    );
    assert!(!error.to_string().contains(TOKEN), "{error}");

    // Nothing listening where the deployment says the container is.
    let gone = serve_plugin("reports", TOKEN);
    let dead_port = gone.port;
    gone.stop();
    platform.deploy(binding, dead_port).await;
    let error = manager.attach_service(binding, None).await.unwrap_err();
    assert!(
        matches!(error, ServiceAttachError::Unreachable { .. }),
        "{error}"
    );

    // A service that declares a different plugin than the one approved.
    let renamed = serve_plugin("invoices", TOKEN);
    platform.deploy(binding, renamed.port).await;
    let error = manager
        .attach_service(binding, Some("reports"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            ServiceAttachError::NameChanged { expected, found, .. }
                if expected == "reports" && found == "invoices"
        ),
        "{error}"
    );

    // A stopped environment is reported as stopped, not as broken.
    platform.set_sleeping(binding, true).await;
    let error = manager.attach_service(binding, None).await.unwrap_err();
    assert!(error.is_asleep(), "{error}");

    assert_eq!(platform.stored("reports").await, None);
    assert_eq!(platform.stored("invoices").await, None);
    assert!(manager.service_plugin_statuses().await.is_empty());
    assert!(manager.manifests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_never_takes_a_plugin_name_installed_from_somewhere_else() {
    let Some(platform) = platform("plugin name ownership").await else {
        return;
    };
    let now = chrono::Utc::now();
    temps_entities::external_plugin_actors::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        plugin_name: Set("reports".into()),
        binary_sha256: Set("0".repeat(64)),
        source_identity: Set("registry:reports".into()),
        active: Set(true),
        ai_tools_enabled: Set(false),
        isolation: Set("container".into()),
        project_id: Set(None),
        environment_id: Set(None),
        manifest: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(platform.db().as_ref())
    .await
    .expect("an installed plugin");
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, plugin.port).await;
    let manager = platform.manager().await;

    let error = manager.attach_service(binding, None).await.unwrap_err();

    assert!(
        matches!(&error, ServiceAttachError::NameTaken { name } if name == "reports"),
        "{error}"
    );
    assert_eq!(
        platform.stored("reports").await,
        Some(("container".to_string(), None, None, false, true)),
        "the installed plugin's row is untouched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_without_deployments_or_tokens_says_why_it_cannot_attach() {
    let Some(platform) = platform("attach prerequisites").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, plugin.port).await;

    let bare = ExternalPluginManager::new(platform.config(), platform.db()).shared();
    assert!(!bare.can_run_services().await);
    let error = bare.attach_service(binding, None).await.unwrap_err();
    assert!(matches!(error, ServiceAttachError::Unavailable), "{error}");

    let tokenless = ExternalPluginManager::new(platform.config(), platform.db()).shared();
    tokenless
        .set_service_runtime(ServiceRuntime::new(
            Arc::new(NoToken),
            RuntimeContext::host(),
        ))
        .await;
    let error = tokenless.attach_service(binding, None).await.unwrap_err();
    assert!(
        matches!(&error, ServiceAttachError::Token { service, .. } if service == "reports-service"),
        "{error}"
    );
    assert_eq!(platform.stored("reports").await, None);
}

/// Records the audit entries the plugin host writes on its own.
#[derive(Default)]
struct HostAudit {
    entries: Mutex<Vec<(String, Option<i32>, String)>>,
}

#[async_trait]
impl ::temps_core::AuditLogger for HostAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn ::temps_core::audit::AuditOperation,
    ) -> ::temps_core::AuditResult<()> {
        self.entries.lock().expect("audit lock").push((
            operation.operation_type(),
            operation.user_id(),
            operation.serialize()?,
        ));
        Ok(())
    }
}

/// The update of `name` that waits, as the database records it.
async fn waiting_update(platform: &Platform, name: &str) -> Option<crate::updates::PendingUpdate> {
    crate::updates::PluginBuilds::new(platform.db())
        .pin_of(name)
        .await
        .expect("read the pin")
        .and_then(|pin| pin.pending)
}

async fn pinned_deployment(platform: &Platform, name: &str) -> Option<i32> {
    crate::updates::PluginBuilds::new(platform.db())
        .pin_of(name)
        .await
        .expect("read the pin")
        .and_then(|pin| pin.deployment_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redeployed_plugin_is_held_until_an_administrator_approves_that_build() {
    let Some(platform) = platform("holding a redeployed plugin").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let first_deployment = platform.deploy(binding, first.port).await;
    let manager = platform.manager().await;
    let audit = Arc::new(HostAudit::default());
    manager.set_audit_service(audit.clone()).await;
    manager.attach_service(binding, None).await.expect("attach");
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment),
        "adding a service pins the build that runs"
    );
    let changed = manager.service_plugins_changed();

    // A deployment replaces the container with a build nobody approved. It
    // asks for exactly what the approved one did.
    let second = serve_plugin("reports", TOKEN);
    let second_deployment = platform.deploy(binding, second.port).await;
    first.stop();

    tokio::time::timeout(Duration::from_secs(10), changed.notified())
        .await
        .expect("the manager reports that the plugin changed");
    eventually("the update waits", || async {
        status_of(&manager, "reports")
            .await
            .is_some_and(|status| status.state == ServicePluginState::UpdateWaiting)
    })
    .await;
    let status = status_of(&manager, "reports").await.expect("listed");
    assert_eq!(status.deployment_id, None, "Temps is attached to nothing");
    let detail = status.detail.expect("why it is not running");
    assert!(
        detail.contains(&format!("deployment {second_deployment}")) && detail.contains("approve"),
        "{detail}"
    );
    // The new build is given no request: the proxy explains instead.
    manager.end_retry_pause("reports").await;
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("three items"), "{body}");
    assert!(body.contains("approved"), "{body}");

    let waiting = waiting_update(&platform, "reports")
        .await
        .expect("the update is recorded");
    assert_eq!(waiting.deployment_id, second_deployment);
    assert!(waiting.since.is_some());
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment),
        "the pin does not move on its own"
    );
    // Held once, on record, naming the plugin and no user; asking again
    // does not repeat it.
    manager.end_retry_pause("reports").await;
    let _ = manager.revive_service("reports").await;
    let held: Vec<_> = audit
        .entries
        .lock()
        .expect("audit lock")
        .iter()
        .filter(|entry| entry.0 == "EXTERNAL_PLUGIN_UPDATE_HELD")
        .cloned()
        .collect();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].1, None);
    let entry: serde_json::Value = serde_json::from_str(&held[0].2).expect("audit json");
    assert_eq!(entry["plugin"], "reports");
    assert_eq!(entry["approved_deployment_id"], first_deployment);
    assert_eq!(entry["deployment_id"], second_deployment);
    assert_eq!(entry["difference"]["asks_for_more"], false);

    // A restarted Temps holds it too, and keeps the plugin's routes.
    let restarted = platform.manager().await;
    let report = restarted.start_service_plugins().await;
    assert_eq!(
        report
            .manifests
            .iter()
            .map(|manifest| manifest.name.as_str())
            .collect::<Vec<_>>(),
        ["reports"]
    );
    assert_eq!(
        status_of(&restarted, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::UpdateWaiting)
    );

    // Approving something else than what waits attaches nothing.
    let refused = manager
        .approve_service_update("reports", "not-the-review-that-was-shown")
        .await
        .expect_err("another review approves nothing");
    assert!(refused.is_update_waiting(), "{refused}");
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment)
    );

    // Approving what waits attaches to it and moves the pin.
    let manifest = manager
        .approve_service_update("reports", &waiting.review)
        .await
        .expect("the reviewed build is attached");
    assert_eq!(manifest.name, "reports");
    let status = status_of(&manager, "reports").await.expect("listed");
    assert_eq!(status.state, ServicePluginState::Running);
    assert_eq!(status.deployment_id, Some(second_deployment));
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(second_deployment)
    );
    assert!(waiting_update(&platform, "reports").await.is_none());
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));
    assert_eq!(
        platform.stored("reports").await.map(|row| row.4),
        Some(true),
        "the same plugin, with the grants it had"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_that_asks_for_more_says_what_and_approving_it_allows_nothing() {
    let Some(platform) = platform("holding an update that asks for more").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, first.port).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");
    let changed = manager.service_plugins_changed();

    let second = serve_plugin_asking("reports", TOKEN, vec![PluginHostPermission::AiGenerate]);
    platform.deploy(binding, second.port).await;
    first.stop();
    tokio::time::timeout(Duration::from_secs(10), changed.notified())
        .await
        .expect("the manager reports that the plugin changed");
    eventually("the update waits", || async {
        waiting_update(&platform, "reports").await.is_some()
    })
    .await;

    let pin = crate::updates::PluginBuilds::new(platform.db())
        .pin_of("reports")
        .await
        .expect("read the pin")
        .expect("the plugin is added");
    let difference = pin.pending_difference().expect("a waiting update");
    assert!(difference.asks_for_more);
    assert_eq!(
        difference.added_permissions,
        [PluginHostPermission::AiGenerate]
    );
    assert!(difference.removed_permissions.is_empty());

    let review = pin.pending.expect("a waiting update").review;
    manager
        .approve_service_update("reports", &review)
        .await
        .expect("the reviewed build is attached");
    // The build runs; what it newly asks for is still off.
    let grants = PluginGrantService::new(platform.db())
        .get("reports")
        .await
        .expect("grants");
    assert!(grants.config.permissions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_approved_build_running_again_is_attached_without_asking() {
    let Some(platform) = platform("rolling a plugin back to its approved build").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let first_deployment = platform.deploy(binding, first.port).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");
    let changed = manager.service_plugins_changed();

    let second = serve_plugin("reports", TOKEN);
    platform.deploy(binding, second.port).await;
    first.stop();
    tokio::time::timeout(Duration::from_secs(10), changed.notified())
        .await
        .expect("the manager reports that the plugin changed");
    eventually("the update waits", || async {
        waiting_update(&platform, "reports").await.is_some()
    })
    .await;

    // The service is rolled back: the approved deployment is current again,
    // in a container started for it.
    let again = serve_plugin("reports", TOKEN);
    platform
        .db()
        .execute_unprepared(&format!(
            "UPDATE deployment_containers SET host_port={} WHERE deployment_id={first_deployment}; \
             UPDATE environments SET current_deployment_id={first_deployment} WHERE id={}",
            again.port, binding.environment_id
        ))
        .await
        .expect("roll back");
    manager.end_retry_pause("reports").await;

    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));
    let status = status_of(&manager, "reports").await.expect("listed");
    assert_eq!(status.state, ServicePluginState::Running);
    assert_eq!(status.deployment_id, Some(first_deployment));
    assert!(
        waiting_update(&platform, "reports").await.is_none(),
        "nothing waits once the approved build runs"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_plugin_keeps_its_routes_and_is_started_by_the_next_request() {
    let Some(platform) = platform("stopping and starting an idle plugin").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let deployment_id = platform.deploy(binding, first.port).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");

    // The environment goes idle: its container stops.
    platform.set_sleeping(binding, true).await;
    first.stop();
    eventually(
        "the plugin is reported as stopped for being idle",
        || async {
            status_of(&manager, "reports")
                .await
                .is_some_and(|status| status.state == ServicePluginState::Asleep)
        },
    )
    .await;
    let status = status_of(&manager, "reports").await.expect("status");
    assert_eq!(status.binding, Some(binding));
    assert!(
        status
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("starts again on the next request")),
        "{status:?}"
    );
    assert!(
        manager
            .manifests()
            .await
            .iter()
            .any(|manifest| manifest.name == "reports"),
        "a stopped plugin keeps its routes and navigation"
    );
    assert!(
        manager.start_failures().await.is_empty(),
        "being idle is not a failure"
    );

    // An anonymous request does not start it.
    let (status, _) = through_proxy(&manager, "reports", "/items", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 0);

    // A start that fails is explained, and the plugin stays listed.
    platform.waker.fail.store(true, AtomicOrdering::SeqCst);
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("did not start within"), "{body}");
    assert!(manager.is_service_plugin("reports").await);
    // A start that just failed is not repeated by the very next request.
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("did not start within"), "{body}");
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(
        status_of(&manager, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::Asleep),
        "it is still only stopped, not broken"
    );
    platform.waker.fail.store(false, AtomicOrdering::SeqCst);
    manager.end_retry_pause("reports").await;

    // The next signed-in request starts the environment and is answered.
    let second = serve_plugin("reports", TOKEN);
    platform.waker.starts_by(vec![
        format!(
            "UPDATE deployment_containers SET host_port={} WHERE deployment_id={deployment_id}",
            second.port
        ),
        format!(
            "UPDATE environments SET sleeping=FALSE WHERE id={}",
            binding.environment_id
        ),
    ]);
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(
        status_of(&manager, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::Running)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_plugin_explains_itself_where_nothing_can_start_it() {
    let Some(platform) = platform("an idle plugin without the on-demand manager").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, plugin.port).await;
    // A process that does not run the proxy has nothing to start services.
    let manager = ExternalPluginManager::new(platform.config(), platform.db()).shared();
    manager
        .set_service_runtime(ServiceRuntime::new(
            Arc::new(FixedToken(TOKEN)),
            RuntimeContext::host(),
        ))
        .await;
    manager.attach_service(binding, None).await.expect("attach");
    platform.set_sleeping(binding, true).await;
    plugin.stop();
    eventually("the plugin is reported as stopped", || async {
        status_of(&manager, "reports")
            .await
            .is_some_and(|status| status.state == ServicePluginState::Asleep)
    })
    .await;

    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.contains("Start the environment from the service's page"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_temps_attaches_again_and_remembers_plugins_it_cannot_reach() {
    let Some(platform) = platform("restarting Temps with service plugins").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, plugin.port).await;
    let before = platform.manager().await;
    before.attach_service(binding, None).await.expect("attach");
    before.stop_supervising_services();
    before.shutdown_all().await;
    drop(before);

    // The plugin kept running while Temps was down and takes the new channel.
    let restarted = platform.manager().await;
    let report = restarted.discover_and_start_report().await;
    assert_eq!(
        report
            .manifests
            .iter()
            .map(|manifest| manifest.name.as_str())
            .collect::<Vec<_>>(),
        ["reports"]
    );
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let (status, body) = through_proxy(&restarted, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));
    restarted.stop_supervising_services();
    restarted.shutdown_all().await;
    drop(restarted);

    // Temps starts while the plugin is stopped for being idle: its routes
    // come from the manifest it last answered with, and it is not a failure.
    platform.set_sleeping(binding, true).await;
    let idle = platform.manager().await;
    let report = idle.discover_and_start_report().await;
    assert_eq!(report.manifests.len(), 1, "{:?}", report.failures);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(
        status_of(&idle, "reports").await.map(|status| status.state),
        Some(ServicePluginState::Asleep)
    );
    assert!(idle.proxy_for("reports").await.is_some());
    drop(idle);

    // Temps starts while the plugin's container is down: the plugin is
    // listed with the reason instead of vanishing.
    platform.set_sleeping(binding, false).await;
    plugin.stop();
    let broken = platform.manager().await;
    let report = broken.discover_and_start_report().await;
    assert!(report.manifests.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].plugin.as_deref(), Some("reports"));
    assert!(
        report.failures[0].reason.contains("did not answer"),
        "{}",
        report.failures[0].reason
    );
    let status = status_of(&broken, "reports").await.expect("still listed");
    assert_eq!(status.state, ServicePluginState::Unavailable);
    assert_eq!(status.binding, Some(binding));
    drop(broken);

    // The service was deleted: the plugin row outlives it and says so.
    platform
        .db()
        .execute_unprepared(&format!(
            "DELETE FROM projects WHERE id={}",
            binding.project_id
        ))
        .await
        .expect("delete the service");
    assert_eq!(
        platform.stored("reports").await,
        Some(("service".to_string(), None, None, true, true))
    );
    let orphaned = platform.manager().await;
    let report = orphaned.discover_and_start_report().await;
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0].reason.contains("was deleted"),
        "{}",
        report.failures[0].reason
    );
    let status = status_of(&orphaned, "reports").await.expect("still listed");
    assert_eq!(status.binding, None);
    assert_eq!(status.state, ServicePluginState::Unavailable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detaching_forgets_the_plugin_and_leaves_its_service_running() {
    let Some(platform) = platform("detaching a service plugin").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    platform.deploy(binding, plugin.port).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");

    assert!(manager.detach_service("reports").await);
    assert!(!manager.detach_service("reports").await, "already detached");

    assert!(!manager.is_service_plugin("reports").await);
    assert!(manager.manifests().await.is_empty());
    assert!(manager.proxy_for("reports").await.is_none());
    // The service is still up and still a plugin anyone may add again.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !manager.is_service_plugin("reports").await,
        "a closed channel does not bring a removed plugin back"
    );
    manager
        .attach_service(binding, None)
        .await
        .expect("the untouched service can be added again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_event_starts_an_idle_plugin_only_when_it_may_receive_events() {
    use crate::event_listener::PluginEventListener;
    use crate::grants::{PluginGrantConfig, PluginGrantService};

    let Some(platform) = platform("delivering an event to an idle plugin").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let deployment_id = platform.deploy(binding, first.port).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");
    platform.set_sleeping(binding, true).await;
    first.stop();
    eventually(
        "the plugin is reported as stopped for being idle",
        || async {
            status_of(&manager, "reports")
                .await
                .is_some_and(|status| status.state == ServicePluginState::Asleep)
        },
    )
    .await;
    let second = serve_plugin("reports", TOKEN);
    platform.waker.starts_by(vec![
        format!(
            "UPDATE deployment_containers SET host_port={} WHERE deployment_id={deployment_id}",
            second.port
        ),
        format!(
            "UPDATE environments SET sleeping=FALSE WHERE id={}",
            binding.environment_id
        ),
    ]);
    let event = temps_plugin_protocol::PluginEvent {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "deployment.succeeded".to_string(),
        timestamp: chrono::Utc::now(),
        project_id: Some(binding.project_id),
        data: serde_json::json!({}),
    };

    // The plugin asks for events but no administrator granted them: an
    // event does not start it.
    assert!(!manager.stopped_service_takes_events("reports").await);
    PluginEventListener::deliver_to_plugins(&manager, &event).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(second.events.load(AtomicOrdering::SeqCst), 0);

    // Once granted, the next event starts the plugin and reaches it.
    PluginGrantService::new(platform.db())
        .update(
            "reports",
            PluginGrantConfig {
                permissions: vec![PluginHostPermission::EventsRead],
                ..Default::default()
            },
        )
        .await
        .expect("grant events");
    assert!(manager.stopped_service_takes_events("reports").await);
    PluginEventListener::deliver_to_plugins(&manager, &event).await;
    eventually("the started plugin is given the event", || async {
        second.events.load(AtomicOrdering::SeqCst) == 1
    })
    .await;
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 1);

    // A start that fails drops the event, and is not repeated for the next
    // one while it has just failed.
    platform.set_sleeping(binding, true).await;
    second.stop();
    eventually("the plugin is stopped again", || async {
        status_of(&manager, "reports")
            .await
            .is_some_and(|status| status.state == ServicePluginState::Asleep)
    })
    .await;
    platform.waker.fail.store(true, AtomicOrdering::SeqCst);
    for _ in 0..2 {
        assert!(
            !PluginEventListener::deliver_after_start(
                manager.clone(),
                "reports".to_string(),
                event.clone()
            )
            .await
        );
    }
    assert_eq!(
        platform.waker.wakes.load(AtomicOrdering::SeqCst),
        2,
        "the second event did not try to start the service again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_plugin_is_cut_off_at_once_and_stays_off_until_it_is_enabled() {
    use crate::event_listener::PluginEventListener;
    use crate::grants::{PluginGrantConfig, PluginGrantService};

    let Some(platform) = platform("disabling a plugin").await else {
        return;
    };
    let plugin = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let deployment_id = platform.deploy(binding, plugin.port).await;
    let administrator = platform.user(7).await;
    let manager = platform.manager().await;
    manager.attach_service(binding, None).await.expect("attach");
    let grants = PluginGrantService::new(platform.db());
    grants
        .update(
            "reports",
            PluginGrantConfig {
                permissions: vec![PluginHostPermission::EventsRead],
                ..Default::default()
            },
        )
        .await
        .expect("grant events");
    let event = temps_plugin_protocol::PluginEvent {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "deployment.succeeded".to_string(),
        timestamp: chrono::Utc::now(),
        project_id: Some(binding.project_id),
        data: serde_json::json!({}),
    };
    PluginEventListener::deliver_to_plugins(&manager, &event).await;
    eventually("the running plugin is given the event", || async {
        plugin.events.load(AtomicOrdering::SeqCst) == 1
    })
    .await;

    assert!(!manager.disable_service("absent").await, "no such plugin");
    assert!(manager.disable_service("reports").await);

    // It stays listed, with its routes, and says why nothing reaches it.
    assert!(manager.is_disabled("reports").await);
    assert!(manager.is_service_plugin("reports").await);
    let status = status_of(&manager, "reports").await.expect("still listed");
    assert_eq!(status.state, ServicePluginState::Disabled);
    assert_eq!(status.binding, Some(binding));
    let detail = status.detail.expect("a disabled plugin says so");
    assert!(
        detail.contains("was disabled by an administrator"),
        "{detail}"
    );
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("was disabled by an administrator"), "{body}");
    assert!(body.contains("Settings > Plugins"), "{body}");

    // No event reaches it or starts it, and it cannot be added again.
    assert!(!manager.stopped_service_takes_events("reports").await);
    PluginEventListener::deliver_to_plugins(&manager, &event).await;
    let error = manager
        .attach_service(binding, None)
        .await
        .expect_err("a disabled plugin is not attached");
    assert!(
        matches!(&error, ServiceAttachError::Disabled { plugin } if plugin == "reports"),
        "{error}"
    );
    // Its service still runs and its channel was closed: nothing connects to
    // it again on its own.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        status_of(&manager, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::Disabled)
    );
    assert_eq!(plugin.events.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 0);

    // A restarted Temps keeps it off, and keeps its routes.
    grants
        .set_disabled("reports", Some(administrator))
        .await
        .expect("record the switch");
    manager.stop_supervising_services();
    manager.shutdown_all().await;
    drop(manager);
    let restarted = platform.manager().await;
    let report = restarted.discover_and_start_report().await;
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(
        report
            .manifests
            .iter()
            .map(|manifest| manifest.name.as_str())
            .collect::<Vec<_>>(),
        ["reports"]
    );
    assert_eq!(
        status_of(&restarted, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::Disabled)
    );
    let (status, _) = through_proxy(&restarted, "reports", "/items", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // Enabled, it is connected to at once: the approved build still runs.
    grants
        .set_disabled("reports", None)
        .await
        .expect("record the switch");
    assert!(!restarted.enable_service("absent").await, "no such plugin");
    assert!(restarted.enable_service("reports").await);
    assert!(!restarted.is_disabled("reports").await);
    let status = status_of(&restarted, "reports").await.expect("listed");
    assert_eq!(status.state, ServicePluginState::Running);
    assert_eq!(status.deployment_id, Some(deployment_id));
    let (status, body) = through_proxy(&restarted, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));

    // Enabling a plugin whose service is stopped for being idle does not
    // start it: the next request does.
    assert!(restarted.disable_service("reports").await);
    platform.set_sleeping(binding, true).await;
    plugin.stop();
    assert!(restarted.enable_service("reports").await);
    assert_eq!(
        status_of(&restarted, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::Asleep)
    );
    assert_eq!(platform.waker.wakes.load(AtomicOrdering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_deployed_from_an_image_is_a_plugin_and_a_new_image_waits() {
    let Some(platform) = platform("a plugin deployed from an image").await else {
        return;
    };
    let first = serve_plugin("reports", TOKEN);
    let binding = platform.service("reports-service").await;
    let first_deployment = platform
        .deploy_image(
            binding,
            first.port,
            "registry.example.com/acme/reports:1.0.0",
        )
        .await;
    let manager = platform.manager().await;

    // No repository and no commit: the image is all there is, and it is
    // enough.
    let manifest = manager
        .attach_service(binding, None)
        .await
        .expect("a service deployed from an image attaches");
    assert_eq!(manifest.name, "reports");
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment),
        "the image deployment that runs is the approved build"
    );
    let (status, body) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "three items"));

    // Deploying another image is an update like any other: it waits.
    let second = serve_plugin("reports", TOKEN);
    let second_deployment = platform
        .deploy_image(
            binding,
            second.port,
            "registry.example.com/acme/reports:1.1.0",
        )
        .await;
    first.stop();
    eventually("the new image is held for review", || async {
        waiting_update(&platform, "reports")
            .await
            .is_some_and(|update| update.deployment_id == second_deployment)
    })
    .await;
    assert_eq!(
        status_of(&manager, "reports")
            .await
            .map(|status| status.state),
        Some(ServicePluginState::UpdateWaiting)
    );
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment),
        "the pin does not follow the new image"
    );

    // The same tag deployed again is still another build.
    let again = serve_plugin("reports", TOKEN);
    let third_deployment = platform
        .deploy_image(
            binding,
            again.port,
            "registry.example.com/acme/reports:1.1.0",
        )
        .await;
    second.stop();
    // Nothing is connected while an update waits, so the next request to
    // the plugin is what finds the newer deployment.
    let (status, _) = through_proxy(&manager, "reports", "/items", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        waiting_update(&platform, "reports")
            .await
            .map(|update| update.deployment_id),
        Some(third_deployment),
        "the redeployed tag is held as its own build"
    );
    assert_eq!(
        pinned_deployment(&platform, "reports").await,
        Some(first_deployment)
    );
}
