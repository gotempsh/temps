// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed access to the platform's HTTP API from a plugin.
//!
//! [`crate::client::TempsClient::api_call`] can reach any endpoint, but it
//! takes a path and a verb as data — which is exactly the stringly-typed hole
//! this protocol set out to remove. This module puts the types back: one
//! entry per endpoint declares its method, its path, its request body and its
//! response, and the call site names none of them.
//!
//! ```rust,no_run
//! # use temps_plugin_sdk::prelude::*;
//! # use temps_plugin_sdk::api::NewProject;
//! # async fn example(
//! #     ctx: &PluginContext,
//! #     auth: &temps_plugin_sdk::protocol::TempsUserContext,
//! # ) -> Result<(), PluginSdkError> {
//! let api = ctx.api_for(auth)?;
//! let project = api.create_project(&NewProject::static_files("demo", "vite")).await?;
//! println!("created project {} ({})", project.id, project.slug);
//! # Ok(())
//! # }
//! ```
//!
//! ## Why this is hand-declared rather than generated
//!
//! The platform's OpenAPI document is produced *from* its Rust handlers by
//! utoipa, so generating a Rust client from it would be a cycle: handlers →
//! spec → client → compiled against those handlers. Breaking the cycle means
//! committing a spec snapshot, which then goes stale silently.
//!
//! So the endpoint table is written by hand and *checked* against the spec
//! instead — see [`DECLARED_ENDPOINTS`] and the conformance test in
//! `temps-cli`. Drift becomes a failing build that names the endpoint, rather
//! than a 404 an operator finds.
//!
//! ## Why the response types are narrower than the API's
//!
//! `ProjectResponse` has thirty fields; a plugin shipping an app reads four.
//! Mirroring the full struct would drag `SourceType`, `DeploymentConfig` and
//! the rest of the platform's internals into every plugin binary, and would
//! break plugins on every additive change to a field they never touch.
//!
//! So each type here is a *projection*: the fields the SDK actually uses,
//! named exactly as the API names them. Serde ignores the rest. The
//! conformance test is what makes that safe — it fails if a projected field
//! stops existing in the spec.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use temps_core::external_plugin::channel::{
    ActorToken, ApiCall, ApiCallResult, BinaryPayload, CallBody, HttpMethod, MultipartPart,
    PartContent, MAX_CALL_BODY_BYTES,
};

use crate::client::TempsClient;
use crate::error::PluginSdkError;

// ── Request bodies ─────────────────────────────────────────────────────

/// Create or replace a project.
///
/// `POST /projects` and `PUT /projects/{id}` take the same body, which is why
/// one type serves both. Only the fields a plugin needs are modelled; the API
/// defaults the rest.
#[derive(Debug, Clone, Serialize)]
pub struct NewProject {
    pub name: String,
    pub main_branch: String,
    pub directory: String,
    pub preset: String,
    pub automatic_deploy: bool,
    pub source_type: String,
    pub storage_service_ids: Vec<i32>,
}

impl NewProject {
    /// A project deployed from uploaded static bundles.
    pub fn static_files(name: impl Into<String>, preset: impl Into<String>) -> Self {
        Self::with_source(name, preset, "static_files")
    }

    /// A project deployed from uploaded container images.
    pub fn docker_image(name: impl Into<String>, preset: impl Into<String>) -> Self {
        Self::with_source(name, preset, "docker_image")
    }

    fn with_source(name: impl Into<String>, preset: impl Into<String>, source_type: &str) -> Self {
        Self {
            name: name.into(),
            main_branch: "main".into(),
            directory: ".".into(),
            preset: preset.into(),
            // A plugin ships explicitly, on its own schedule. Leaving
            // auto-deploy on would let an unrelated git push overwrite what
            // the plugin just deployed.
            automatic_deploy: false,
            source_type: source_type.into(),
            storage_service_ids: Vec::new(),
        }
    }
}

/// Create a managed service (a database, cache, bucket) on the platform.
#[derive(Debug, Clone, Serialize)]
pub struct NewService {
    pub name: String,
    /// Platform service type, e.g. `postgres`, `redis`, `mongodb`.
    pub service_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Type-specific parameters, e.g. `database` and `username` for Postgres.
    pub parameters: std::collections::HashMap<String, String>,
    /// `standalone` unless the caller is building a cluster.
    pub topology: String,
    /// Cluster member specifications. Required when `topology` is `cluster`.
    pub members: Vec<ClusterMemberRequest>,
}

/// Placement and service-specific role for one managed-service cluster member.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterMemberRequest {
    /// Service-type-specific role, such as `primary`, `replica`, or `monitor`.
    pub role: String,
    /// Worker node on which to place the member, or the control plane when absent.
    pub node_id: Option<i32>,
}

impl NewService {
    /// A standalone Postgres with an application database and role.
    pub fn postgres(name: impl Into<String>, database: &str, username: &str) -> Self {
        let mut parameters = std::collections::HashMap::new();
        parameters.insert("database".to_string(), database.to_string());
        parameters.insert("username".to_string(), username.to_string());
        Self {
            name: name.into(),
            service_type: "postgres".into(),
            version: None,
            parameters,
            topology: "standalone".into(),
            members: Vec::new(),
        }
    }
}

/// Link an existing managed service to a project.
#[derive(Debug, Clone, Serialize)]
pub struct LinkService {
    pub project_id: i32,
}

/// Start a deployment from an already-uploaded static bundle.
#[derive(Debug, Clone, Serialize)]
pub struct DeployStaticBundle {
    pub static_bundle_id: i32,
}

/// Start a deployment from a container image the platform can already reach.
///
/// Set `claim_local` only for an image built in the platform host's Docker.
/// Temps inspects and retags that image into a project-owned internal name,
/// then verifies its immutable image ID before deployment. Registry images
/// must leave it false and are never resolved from local daemon state.
#[derive(Debug, Clone, Serialize)]
pub struct DeployImage {
    pub image_ref: String,
    #[serde(default)]
    pub claim_local: bool,
}

// ── Response projections ───────────────────────────────────────────────

/// A narrow view of one of the API's response types.
///
/// Declaring the schema name and the field list as data is what lets the
/// conformance test check them: it can look the schema up in the platform's
/// OpenAPI document and verify every projected field is really there, and
/// really required. Without this the test could only check that the *path*
/// exists, which would not have caught a deploy endpoint returning `state`
/// where the SDK expected `status`.
pub trait ApiProjection {
    /// The OpenAPI component schema this projects.
    const SCHEMA: &'static str;
    /// Fields read from that schema, and whether each must be present.
    ///
    /// A field marked required must be in the schema's `required` list — a
    /// non-`Option` Rust field cannot decode a response that omits it.
    const FIELDS: &'static [(&'static str, bool)];
}

/// Define a projection and its conformance metadata from one field list, so
/// the struct and the list it is checked against cannot drift.
macro_rules! projection {
    (
        $(#[$sdoc:meta])*
        $name:ident of $schema:literal {
            $(
                $(#[$fdoc:meta])*
                $field:ident: $ty:ty, required = $required:literal;
            )*
        }
    ) => {
        $(#[$sdoc])*
        #[derive(Debug, Clone, Deserialize)]
        pub struct $name {
            $(
                $(#[$fdoc])*
                pub $field: $ty,
            )*
        }

        impl ApiProjection for $name {
            const SCHEMA: &'static str = $schema;
            const FIELDS: &'static [(&'static str, bool)] =
                &[$( (stringify!($field), $required), )*];
        }
    };
}

projection! {
    /// The fields of `ProjectResponse` a plugin needs to deploy into a project.
    ProjectRef of "ProjectResponse" {
        id: i32, required = true;
        name: String, required = true;
        slug: String, required = true;
        /// `None` for projects created before presets existed.
        preset: Option<String>, required = false;
        repo_name: Option<String>, required = false;
        repo_owner: Option<String>, required = false;
        git_url: Option<String>, required = false;
        main_branch: String, required = true;
        last_deployment: Option<i64>, required = false;
        created_at: i64, required = true;
    }
}

/// Caller-filtered page returned by `GET /projects`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectPage {
    pub projects: Vec<ProjectRef>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

projection! {
    /// The fields of `EnvironmentResponse` a plugin needs to deploy and to
    /// tell the user where the result lives.
    EnvironmentRef of "EnvironmentResponse" {
        id: i32, required = true;
        project_id: i32, required = true;
        name: String, required = true;
        slug: String, required = true;
        /// Browser-reachable URL for this environment.
        main_url: String, required = true;
    }
}

projection! {
    /// The fields of `RemoteDeploymentResponse` — what the *deploy* endpoints
    /// return.
    ///
    /// Deliberately separate from [`DeploymentRef`]: a started deployment
    /// reports `state` and has a `slug`, while polling one reports `status`
    /// and has a `url`. They are different types on the platform, and
    /// pretending otherwise produced a decode failure that only appeared on a
    /// real deploy.
    StartedDeployment of "RemoteDeploymentResponse" {
        id: i32, required = true;
        project_id: i32, required = true;
        environment_id: i32, required = true;
        slug: String, required = true;
        state: String, required = true;
    }
}

projection! {
    /// The fields of `DeploymentResponse` a plugin needs to follow a
    /// deployment to its conclusion.
    DeploymentRef of "DeploymentResponse" {
        id: i32, required = true;
        status: String, required = true;
        url: String, required = true;
        /// Set when `status` is a failure — the platform's own explanation,
        /// which is the only thing that tells a user *why* their deploy
        /// failed.
        cancelled_reason: Option<String>, required = false;
    }
}

projection! {
    /// The fields of `StaticBundleResponse` a plugin needs to deploy a bundle
    /// it just uploaded.
    StaticBundleRef of "StaticBundleResponse" {
        id: i32, required = true;
        project_id: i32, required = true;
        size_bytes: i64, required = true;
    }
}

impl DeploymentRef {
    /// Whether this deployment reached a terminal state.
    pub fn is_finished(&self) -> bool {
        self.is_success() || self.is_failure()
    }

    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "success" | "completed" | "deployed")
    }

    pub fn is_failure(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "error" | "cancelled")
    }
}

projection! {
    /// The fields of `ExternalServiceInfo` a plugin needs after creating a
    /// managed service.
    ///
    /// `service_type` is deliberately absent: the platform types it as an
    /// enum (`ServiceTypeRoute`), and a plugin that projected it as a
    /// `String` would break the moment a new service type is added. The
    /// caller already knows what it asked for.
    ServiceRef of "ExternalServiceInfo" {
        id: i32, required = true;
        name: String, required = true;
        status: String, required = true;
    }
}

projection! {
    /// The fields of `ProjectServiceInfo` returned when a service is linked
    /// to a project.
    ///
    /// Only the link's own id: the nested `project` and `service` objects
    /// echo back what the caller already had.
    ServiceLinkRef of "ProjectServiceInfo" {
        id: i32, required = true;
    }
}

/// Live connection variables for a managed service in one environment.
///
/// Not a `projection!`: the platform returns a free-form map keyed by
/// variable name (`POSTGRES_URL`, `POSTGRES_PASSWORD`, ...), so there are no
/// fixed fields for the conformance test to check. The endpoint itself is in
/// [`DECLARED_ENDPOINTS`], which is what would catch it moving.
#[derive(Deserialize)]
pub struct RuntimeCredentials {
    pub variables: std::collections::HashMap<String, String>,
}

impl RuntimeCredentials {
    /// The variable a framework is most likely to want, under the name most
    /// frameworks expect.
    ///
    /// Temps emits `POSTGRES_URL`; almost every ORM reads `DATABASE_URL`.
    /// Translating here rather than in each plugin means one place gets it
    /// wrong instead of all of them.
    pub fn database_url(&self) -> Option<&str> {
        self.variables
            .get("DATABASE_URL")
            .or_else(|| self.variables.get("POSTGRES_URL"))
            .map(String::as_str)
    }

    /// Variable names only, for logging. There is no accessor that renders
    /// the values, on purpose.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.variables.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

/// Debug prints names only — these are live credentials.
impl std::fmt::Debug for RuntimeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RuntimeCredentials {{ variables: {:?} }}", self.names())
    }
}

/// Every projection this SDK declares, as `(schema, fields)`.
///
/// The conformance test walks this against the platform's OpenAPI components.
pub const DECLARED_PROJECTIONS: &[(&str, &[(&str, bool)])] = &[
    (ProjectRef::SCHEMA, ProjectRef::FIELDS),
    (EnvironmentRef::SCHEMA, EnvironmentRef::FIELDS),
    (StartedDeployment::SCHEMA, StartedDeployment::FIELDS),
    (DeploymentRef::SCHEMA, DeploymentRef::FIELDS),
    (StaticBundleRef::SCHEMA, StaticBundleRef::FIELDS),
    (ServiceRef::SCHEMA, ServiceRef::FIELDS),
    (ServiceLinkRef::SCHEMA, ServiceLinkRef::FIELDS),
];

// ── The client ─────────────────────────────────────────────────────────

/// Typed platform API bound to one acting user.
///
/// Obtained from [`crate::context::PluginContext::api_for`], which supplies
/// the actor token the platform minted for the request being served. There is
/// no way to build one without an actor, so a plugin cannot accidentally make
/// an unattributed call.
#[derive(Clone)]
pub struct PlatformApi {
    client: TempsClient,
    actor: ActorToken,
}

impl PlatformApi {
    pub(crate) fn new(client: TempsClient, actor: ActorToken) -> Self {
        Self { client, actor }
    }

    /// List projects visible to this caller.
    ///
    /// This intentionally uses the platform's HTTP handler rather than the
    /// legacy channel `ListProjects` call: the handler applies project-access
    /// filtering and the caller's resolved `projects:read` permission.
    pub async fn list_projects(
        &self,
        page: i64,
        per_page: i64,
    ) -> Result<ProjectPage, PluginSdkError> {
        self.send(
            HttpMethod::Get,
            "/projects".to_string(),
            Some(format!("page={page}&per_page={per_page}")),
            None,
        )
        .await
    }

    /// Issue a request and decode the response body.
    async fn send<Res: DeserializeOwned>(
        &self,
        method: HttpMethod,
        path: String,
        query: Option<String>,
        body: Option<CallBody>,
    ) -> Result<Res, PluginSdkError> {
        if let Some(body) = &body {
            // Checked here as well as on the platform so an oversized upload
            // fails before it is base64-encoded and pushed through a socket,
            // which on a big artifact is the difference between an instant
            // error and a multi-second stall ending in the same error.
            let len = body.payload_len();
            if len > MAX_CALL_BODY_BYTES {
                return Err(PluginSdkError::BodyTooLarge {
                    len,
                    limit: MAX_CALL_BODY_BYTES,
                    method: method.as_str(),
                    path,
                });
            }
        }

        let result: ApiCallResult = self
            .client
            .api_call(ApiCall {
                method,
                path: path.clone(),
                query,
                body,
                actor: self.actor.clone(),
            })
            .await?;

        if !result.is_success() {
            // Surface the platform's own words. A plugin author debugging a
            // rejected call needs the API's problem-details body, not
            // "request failed".
            return Err(PluginSdkError::ApiStatus {
                status: result.status,
                method: method.as_str(),
                path,
                body: result.body.as_str().to_string(),
            });
        }

        result
            .body
            .decode()
            .map_err(|e| PluginSdkError::Deserialization {
                reason: e.to_string(),
            })
    }

    /// Upload a static bundle (a `.tar.gz` of the built output) to a project.
    ///
    /// Not part of [`define_api!`] because the endpoint takes
    /// `multipart/form-data` rather than JSON, and because the archive is the
    /// one payload big enough to be worth naming its own limit.
    pub async fn upload_static_bundle(
        &self,
        project_id: i32,
        filename: &str,
        archive: Vec<u8>,
    ) -> Result<StaticBundleRef, PluginSdkError> {
        let parts = vec![
            MultipartPart {
                name: "file".into(),
                filename: Some(filename.to_string()),
                content_type: Some("application/gzip".into()),
                content: PartContent::Binary(BinaryPayload::new(archive)),
            },
            MultipartPart {
                name: "content_type".into(),
                filename: None,
                content_type: None,
                content: PartContent::Text("application/gzip".into()),
            },
        ];
        self.send(
            HttpMethod::Post,
            format!("/projects/{project_id}/upload/static"),
            None,
            Some(CallBody::Multipart(parts)),
        )
        .await
    }

    /// Deploy a container image saved with `docker save`.
    ///
    /// `tag` is the image tag inside the tarball, which the platform needs in
    /// order to know which image it just imported.
    pub async fn deploy_image_upload(
        &self,
        project_id: i32,
        environment_id: i32,
        tag: &str,
        image_tar: Vec<u8>,
    ) -> Result<StartedDeployment, PluginSdkError> {
        let parts = vec![MultipartPart {
            name: "file".into(),
            filename: Some("image.tar".into()),
            content_type: Some("application/x-tar".into()),
            content: PartContent::Binary(BinaryPayload::new(image_tar)),
        }];
        self.send(
            HttpMethod::Post,
            format!("/projects/{project_id}/environments/{environment_id}/deploy/image-upload"),
            Some(format!("tag={}", urlencode(tag))),
            Some(CallBody::Multipart(parts)),
        )
        .await
    }
}

/// Percent-encode a query-string value.
///
/// Hand-rolled rather than pulled in as a dependency: the SDK is compiled
/// into every plugin binary, and this is the only place it needs escaping.
/// The unreserved set is RFC 3986 §2.3.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Declare an endpoint: its verb, its path, its parameters, its body and its
/// response.
///
/// The generated method fills in everything but the caller's data, so no call
/// site ever spells a path or a verb. Path parameters are named the same as
/// in the OpenAPI path template, so the `format!` below captures them
/// implicitly and the declared path doubles as the conformance key.
macro_rules! define_api {
    (
        $(
            $(#[$doc:meta])*
            $name:ident($method:ident $path:literal $(, $param:ident: $ptype:ty)*
                        $(, @body $req:ty)?) -> $res:ty;
        )*
    ) => {
        impl PlatformApi {
            $(
                $(#[$doc])*
                pub async fn $name(
                    &self,
                    $($param: $ptype,)*
                    $(body: &$req,)?
                ) -> Result<$res, PluginSdkError> {
                    // Written only by the optional body arm below; the
                    // allow covers the bodyless endpoints, where the macro
                    // expands to a plain `None`.
                    #[allow(unused_mut, unused_assignments)]
                    let mut encoded: Option<CallBody> = None;
                    $(
                        let typed: &$req = body;
                        encoded = Some(CallBody::json(typed).map_err(|e| {
                            PluginSdkError::Deserialization { reason: e.to_string() }
                        })?);
                    )?
                    self.send(HttpMethod::$method, format!($path), None, encoded).await
                }
            )*
        }

        /// Every endpoint this SDK claims the platform has, as
        /// `(METHOD, path-template)`.
        ///
        /// Checked against the platform's OpenAPI document by the conformance
        /// test in `temps-cli`, so a renamed route or a changed verb fails the
        /// build instead of 404ing at runtime.
        pub const DECLARED_ENDPOINTS: &[(&str, &str)] = &[
            $( (stringify!($method), $path), )*
            // The multipart endpoints are hand-written above but still belong
            // in the table — they are exactly the ones whose breakage would be
            // hardest to spot, since they only run on a real deploy.
            ("POST", "/projects/{project_id}/upload/static"),
            ("POST", "/projects/{project_id}/environments/{environment_id}/deploy/image-upload"),
            ("GET", "/projects"),
        ];
    };
}

define_api! {
    /// Create a project.
    create_project(Post "/projects", @body NewProject) -> ProjectRef;

    /// Fetch a project by ID.
    get_project(Get "/projects/{id}", id: i32) -> ProjectRef;

    /// Replace a project's settings — notably its source type, which decides
    /// whether it deploys static bundles or container images.
    update_project(Put "/projects/{id}", id: i32, @body NewProject) -> ProjectRef;

    /// List a project's environments. The first is its default.
    list_environments(Get "/projects/{project_id}/environments", project_id: i32)
        -> Vec<EnvironmentRef>;

    /// Start a deployment from an uploaded static bundle.
    deploy_static(
        Post "/projects/{project_id}/environments/{environment_id}/deploy/static",
        project_id: i32,
        environment_id: i32,
        @body DeployStaticBundle
    ) -> StartedDeployment;

    /// Start a deployment from a named container image.
    ///
    /// Prefer this over [`PlatformApi::deploy_image_upload`] whenever the
    /// platform can reach the image by name: it sends a tag instead of a
    /// tarball, so it is not bounded by the channel's body cap.
    deploy_image(
        Post "/projects/{project_id}/environments/{environment_id}/deploy/image",
        project_id: i32,
        environment_id: i32,
        @body DeployImage
    ) -> StartedDeployment;

    /// Create a managed service (database, cache, bucket).
    create_service(Post "/external-services", @body NewService) -> ServiceRef;

    /// Link a managed service to a project so deployments can reach it.
    link_service(
        Post "/external-services/{id}/projects",
        id: i32,
        @body LinkService
    ) -> ServiceLinkRef;

    /// Issue live connection credentials, provisioning the per-tenant
    /// database if it does not exist yet.
    ///
    /// A POST because it is not a read: it creates a database as a side
    /// effect, and its response is a live credential. Every call is audited
    /// on the platform against the acting user.
    issue_runtime_credentials(
        Post "/external-services/{id}/projects/{project_id}/environments/{environment_id}/runtime-credentials",
        id: i32,
        project_id: i32,
        environment_id: i32
    ) -> RuntimeCredentials;

    /// Fetch a deployment, to poll it to completion.
    get_deployment(
        Get "/projects/{project_id}/deployments/{deployment_id}",
        project_id: i32,
        deployment_id: i32
    ) -> DeploymentRef;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table must not contain duplicates: two entries for one endpoint
    /// means one of them is dead and nobody would notice.
    #[test]
    fn declared_endpoints_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in DECLARED_ENDPOINTS {
            assert!(seen.insert(entry), "duplicate endpoint declared: {entry:?}");
        }
    }

    /// Paths are relative to `/api` and must not embed it: the bridge
    /// prefixes `/api`, so a path that already has it would resolve to
    /// `/api/api/...` and 404.
    #[test]
    fn declared_paths_are_relative_to_the_api_root() {
        for (method, path) in DECLARED_ENDPOINTS {
            assert!(path.starts_with('/'), "{method} {path} must start with '/'");
            assert!(
                !path.starts_with("/api/"),
                "{method} {path} must not include the /api prefix; the bridge adds it"
            );
        }
    }

    /// Every declared verb must be one the channel can carry, or the call
    /// would fail at the far end with an unhelpful parse error.
    #[test]
    fn declared_methods_are_transportable() {
        for (method, path) in DECLARED_ENDPOINTS {
            assert!(
                matches!(
                    *method,
                    "Get" | "Post" | "Put" | "Patch" | "Delete" | "GET" | "POST"
                ),
                "{method} {path} is not a verb HttpMethod can express"
            );
        }
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(
            urlencode("plugin-demo:1730000000"),
            "plugin-demo%3A1730000000"
        );
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("plain-Name_1.0~x"), "plain-Name_1.0~x");
    }

    /// A project the plugin creates must never inherit git auto-deploy: an
    /// unrelated push would overwrite what the plugin just shipped.
    #[test]
    fn new_projects_never_auto_deploy() {
        assert!(!NewProject::static_files("demo", "vite").automatic_deploy);
        assert!(!NewProject::docker_image("demo", "dockerfile").automatic_deploy);
    }

    #[test]
    fn cluster_members_serialize_with_role_and_optional_node_id() {
        let mut service = NewService::postgres("database", "app", "app");
        service.topology = "cluster".to_string();
        service.members = vec![
            ClusterMemberRequest {
                role: "primary".to_string(),
                node_id: Some(7),
            },
            ClusterMemberRequest {
                role: "replica".to_string(),
                node_id: None,
            },
        ];

        let json = serde_json::to_value(service).expect("service request should serialize");
        assert_eq!(
            json["members"],
            serde_json::json!([
                {"role": "primary", "node_id": 7},
                {"role": "replica", "node_id": null}
            ])
        );
    }

    #[test]
    fn deployment_terminal_states_are_recognised() {
        let at = |status: &str| DeploymentRef {
            id: 1,
            status: status.into(),
            url: String::new(),
            cancelled_reason: None,
        };
        assert!(at("success").is_success() && at("success").is_finished());
        assert!(at("failed").is_failure() && at("failed").is_finished());
        assert!(!at("building").is_finished());
        assert!(!at("queued").is_finished());
    }

    /// The projections must survive a response carrying the API's full field
    /// set — that is the whole premise of declaring them narrow.
    #[test]
    fn projections_ignore_the_fields_they_do_not_declare() {
        let full = r#"{
            "id": 7, "slug": "demo", "name": "Demo", "preset": "vite",
            "repo_name": null, "repo_owner": null, "directory": ".",
            "main_branch": "main", "created_at": 0, "updated_at": 0,
            "attack_mode": false, "source_type": "static_files",
            "enable_preview_environments": false
        }"#;
        let project: ProjectRef = serde_json::from_str(full).unwrap();
        assert_eq!(project.id, 7);
        assert_eq!(project.preset.as_deref(), Some("vite"));
    }

    /// A deployment that has not failed omits `cancelled_reason` entirely in
    /// some responses; that must not be a decode error.
    #[test]
    fn missing_cancelled_reason_decodes_as_none() {
        let json = r#"{"id":3,"status":"building","url":"https://x.test"}"#;
        let deployment: DeploymentRef = serde_json::from_str(json).unwrap();
        assert!(deployment.cancelled_reason.is_none());
    }
}
