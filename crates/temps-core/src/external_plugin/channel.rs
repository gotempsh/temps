//! Platform channel protocol types.
//!
//! Defines the bidirectional message protocol used between Temps and
//! external plugins over a WebSocket connection.  Temps initiates the
//! connection to the plugin's `/_temps/channel` endpoint after the handshake
//! completes.
//!
//! ## Message flow
//!
//! ```text
//! Plugin (client role)                Temps (server role)
//!     │                                    │
//!     │─── Request { id, call } ──────────>│  plugin asks for data
//!     │<── Response { id, outcome } ───────│  platform responds
//!     │                                    │
//!     │<── Event { event }  ───────────────│  platform pushes event
//!     │                                    │
//! ```
//!
//! ## Why this is typed rather than `method: String` + `params: Value`
//!
//! The first version of this protocol carried a method name and a
//! `serde_json::Value`.  Both sides then hand-built and hand-parsed JSON, so
//! a renamed field or a misspelled method compiled cleanly on both sides and
//! failed at runtime, on an operator's machine, as a `MethodNotFound` or a
//! silent `null`.
//!
//! Now a call is a variant of [`PlatformCallRequest`] and its reply is the
//! matching variant of [`PlatformCallResponse`], paired at compile time by
//! the [`PlatformCall`] trait.  Adding a call without handling it is a
//! non-exhaustive-match error in the platform's dispatcher rather than a
//! runtime surprise.  Serde still puts the bytes on the wire — this is a
//! JSON WebSocket — but no `serde_json::Value` appears in any signature.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

// ── Wire messages ──────────────────────────────────────────────────────

/// Top-level envelope for all messages on the channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelMessage {
    /// Plugin → Platform: request data or perform an API call.
    Request(ChannelRequest),
    /// Platform → Plugin: response to a request.
    Response(ChannelResponse),
    /// Platform → Plugin: pushed event (fire-and-forget).
    Event(ChannelEvent),
}

/// A request from the plugin to the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRequest {
    /// Caller-assigned correlation ID (echoed in the response).
    pub id: u64,
    /// The typed call being made.
    pub call: PlatformCallRequest,
}

/// The platform's response to a [`ChannelRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelResponse {
    /// Correlation ID from the original request.
    pub id: u64,
    /// Success payload or structured error.
    pub outcome: CallOutcome,
}

/// Either the typed result of a call, or why it could not be served.
///
/// The success payload is boxed because it is an order of magnitude larger
/// than the error: every `ChannelResponse` — including the error ones, which
/// on a failing plugin are most of them — would otherwise carry the footprint
/// of the biggest response variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Ok(Box<PlatformCallResponse>),
    Err(ChannelError),
}

/// A platform-pushed event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub event: super::PluginEvent,
}

/// Structured error returned inside a [`ChannelResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelError {
    /// Machine-readable error code.
    pub code: ChannelErrorCode,
    /// Human-readable description.
    pub message: String,
}

impl ChannelError {
    pub fn new(code: ChannelErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

/// Error codes for the channel protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelErrorCode {
    /// The requested method does not exist on this platform build.
    MethodNotFound,
    /// The parameters are invalid or missing required fields.
    InvalidParams,
    /// The plugin did not declare the capability this call requires, or the
    /// acting user lacks the permission.
    PermissionDenied,
    /// The plugin presented no usable actor token for a call that acts on a
    /// user's behalf.
    Unauthenticated,
    /// The requested resource was not found.
    NotFound,
    /// An internal platform error occurred.
    Internal,
}

// ── Typed API calls ────────────────────────────────────────────────────

/// HTTP method for an [`ApiCall`].
///
/// An enum rather than a string, so a plugin cannot invent a verb the router
/// will never match, and so a capability check can tell a read from a write
/// without parsing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    /// Whether this verb can change state.
    ///
    /// Drives capability selection: a read needs only the read capability,
    /// anything else needs the write one.
    pub fn is_mutating(self) -> bool {
        !matches!(self, HttpMethod::Get)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }
}

/// A JSON document in transit.
///
/// Holds already-encoded JSON rather than a `serde_json::Value`, so the
/// protocol has no untyped hole in it: the typed layer on each side encodes
/// and decodes concrete structs, and this is only the envelope carrying the
/// bytes between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JsonBody(String);

impl JsonBody {
    /// Encode a typed value for transport.
    pub fn encode<T: Serialize>(value: &T) -> Result<Self, JsonBodyError> {
        serde_json::to_string(value)
            .map(Self)
            .map_err(|e| JsonBodyError::Encode {
                type_name: std::any::type_name::<T>(),
                reason: e.to_string(),
            })
    }

    /// Decode into the concrete type the caller expects.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, JsonBodyError> {
        serde_json::from_str(&self.0).map_err(|e| JsonBodyError::Decode {
            type_name: std::any::type_name::<T>(),
            reason: e.to_string(),
            // Bodies carry credentials and customer data, so the payload
            // itself is never put in the error.
            len: self.0.len(),
        })
    }

    /// Wrap raw JSON text produced elsewhere (e.g. an HTTP response body).
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0.into_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Why a [`JsonBody`] could not be encoded or decoded.
#[derive(Debug, Clone, thiserror::Error)]
pub enum JsonBodyError {
    #[error("Failed to encode {type_name} for the platform channel: {reason}")]
    Encode {
        type_name: &'static str,
        reason: String,
    },
    #[error("Failed to decode {len}-byte channel payload as {type_name}: {reason}")]
    Decode {
        type_name: &'static str,
        reason: String,
        len: usize,
    },
}

/// Proof that a plugin is acting for a specific signed-in user.
///
/// Minted by the platform when it proxies an authenticated request to the
/// plugin, and echoed back by the plugin on any call made on that user's
/// behalf. A plugin cannot forge one — it is signed with the instance's auth
/// secret — so a plugin can never act as a user who did not call it, nor
/// exceed that user's own permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorToken(String);

impl ActorToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque binary payload, base64 on the wire.
///
/// The channel is a JSON WebSocket, so raw bytes need an encoding. Base64
/// rather than a `Vec<u8>` because serde_json renders a byte vector as an
/// array of decimal numbers — roughly 4x the size of base64 and far slower to
/// parse.
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryPayload(Vec<u8>);

impl BinaryPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Debug prints the length only. Uploads are customer code and customer data;
/// a `{:?}` in a log line must never dump them.
impl std::fmt::Debug for BinaryPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BinaryPayload({} bytes)", self.0.len())
    }
}

impl Serialize for BinaryPayload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for BinaryPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map(Self)
            // The bad input is not echoed — it may be a partially-decoded
            // secret. The length is enough to tell truncation from garbage.
            .map_err(|e| {
                serde::de::Error::custom(format!(
                    "Channel payload of {} base64 characters is not valid base64: {e}",
                    encoded.len()
                ))
            })
    }
}

/// One part of a `multipart/form-data` request body.
///
/// Declared as data rather than assembled by the plugin, so no plugin ever
/// writes a MIME boundary — the platform builds the wire format, which means
/// a boundary can never collide with the payload or be omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultipartPart {
    /// Form field name.
    pub name: String,
    /// Filename, for parts the handler treats as an upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub content: PartContent,
}

/// A multipart part's payload: text for form fields, bytes for uploads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PartContent {
    Text(String),
    Binary(BinaryPayload),
}

impl PartContent {
    pub fn len(&self) -> usize {
        match self {
            PartContent::Text(t) => t.len(),
            PartContent::Binary(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The request body of an [`ApiCall`].
///
/// Two shapes because the platform's API genuinely has two: most endpoints
/// take JSON, but bundle and image uploads take `multipart/form-data`. A
/// plugin restricted to JSON could not deploy anything, and one handed a raw
/// byte buffer plus a content-type would be back to hand-building MIME.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum CallBody {
    Json(JsonBody),
    Multipart(Vec<MultipartPart>),
}

impl CallBody {
    /// Encode a typed value as a JSON body.
    pub fn json<T: Serialize>(value: &T) -> Result<Self, JsonBodyError> {
        JsonBody::encode(value).map(CallBody::Json)
    }

    /// Total payload size, before MIME framing.
    ///
    /// Used to reject an over-large body at the call site rather than after
    /// it has been serialized, buffered and pushed through the socket.
    pub fn payload_len(&self) -> usize {
        match self {
            CallBody::Json(b) => b.as_str().len(),
            CallBody::Multipart(parts) => parts.iter().map(|p| p.content.len()).sum(),
        }
    }
}

/// Largest request payload a plugin may put on the channel.
///
/// The body is base64-encoded into a JSON frame and buffered whole on both
/// sides, so it costs roughly 4/3 of this in transit and again in the
/// platform's memory. On the reference 4 GB box that is already generous for
/// a static bundle, which is what this exists for. Anything larger — a saved
/// Docker image, a database dump — needs a streaming path, not a bigger
/// number here.
pub const MAX_CALL_BODY_BYTES: usize = 32 * 1024 * 1024;

/// A call into the platform's own HTTP API, carried over the channel.
///
/// The platform dispatches this into the very router that serves the
/// console, so a plugin reaches exactly the endpoints the product has, with
/// the same handlers, the same `permission_guard!` checks and the same audit
/// logging. There is deliberately no parallel API to keep in sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCall {
    pub method: HttpMethod,
    /// API path *below* `/api`, e.g. `/projects/42`.
    pub path: String,
    /// Raw query string, without the leading `?`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CallBody>,
    /// Who the plugin is acting for.
    pub actor: ActorToken,
}

/// In-process proof that a platform HTTP request originated from a verified
/// external-plugin channel call.
///
/// This is inserted into request extensions only after the channel has
/// verified the plugin-bound actor token. HTTP headers cannot create it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPluginApiCaller {
    plugin_name: String,
}

impl VerifiedPluginApiCaller {
    pub fn new(plugin_name: impl Into<String>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
        }
    }

    pub fn plugin_name(&self) -> &str {
        &self.plugin_name
    }
}

/// The platform's reply to an [`ApiCall`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCallResult {
    pub status: u16,
    pub body: JsonBody,
}

impl ApiCallResult {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

// ── Call parameters ────────────────────────────────────────────────────

/// Parameters for [`PlatformCallRequest::GetProject`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetProject {
    pub project_id: i32,
}

/// Parameters for [`PlatformCallRequest::ListProjects`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListProjects {}

/// Parameters for [`PlatformCallRequest::GetEnvironment`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetEnvironment {
    pub environment_id: i32,
}

/// Parameters for [`PlatformCallRequest::ListEnvironments`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListEnvironments {
    pub project_id: i32,
}

/// Parameters for [`PlatformCallRequest::GetDeployment`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetDeployment {
    pub deployment_id: i32,
}

/// Parameters for [`PlatformCallRequest::GetLastDeployment`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetLastDeployment {
    pub project_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,
}

/// Parameters for [`PlatformCallRequest::ListDeployments`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDeployments {
    pub project_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

// ── Call/response pairing ──────────────────────────────────────────────

/// Generate the request/response enums and their compile-time pairing.
///
/// One entry per call is the whole definition: both wire enums, the
/// [`PlatformCall`] impl saying which reply belongs to which request, and
/// the reply extraction. Keeping them in a single macro is what makes it
/// impossible to add a request without its matching response variant.
macro_rules! define_platform_calls {
    (
        $(
            $(#[$doc:meta])*
            $variant:ident($params:ty) -> $output:ty,
        )*
    ) => {
        /// A typed call from a plugin to the platform.
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(tag = "method", content = "params", rename_all = "snake_case")]
        pub enum PlatformCallRequest {
            $(
                $(#[$doc])*
                $variant($params),
            )*
        }

        /// The platform's typed reply, one variant per [`PlatformCallRequest`].
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(tag = "method", content = "result", rename_all = "snake_case")]
        pub enum PlatformCallResponse {
            $(
                $(#[$doc])*
                $variant($output),
            )*
        }

        impl PlatformCallRequest {
            /// Method name, for logging and error messages only — never for
            /// dispatch, which matches on the variant.
            pub fn method_name(&self) -> &'static str {
                match self {
                    $( PlatformCallRequest::$variant(_) => stringify!($variant), )*
                }
            }
        }

        impl PlatformCallResponse {
            pub fn method_name(&self) -> &'static str {
                match self {
                    $( PlatformCallResponse::$variant(_) => stringify!($variant), )*
                }
            }
        }

        $(
            impl PlatformCall for $params {
                type Output = $output;

                fn into_request(self) -> PlatformCallRequest {
                    PlatformCallRequest::$variant(self)
                }

                fn output_from(
                    response: PlatformCallResponse,
                ) -> Result<Self::Output, ProtocolMismatch> {
                    match response {
                        PlatformCallResponse::$variant(out) => Ok(out),
                        other => Err(ProtocolMismatch {
                            expected: stringify!($variant),
                            received: other.method_name(),
                        }),
                    }
                }
            }
        )*
    };
}

define_platform_calls! {
    /// Fetch one project by ID.
    GetProject(GetProject) -> ProjectInfo,
    /// List every non-deleted project.
    ListProjects(ListProjects) -> Vec<ProjectInfo>,
    /// Fetch one environment by ID.
    GetEnvironment(GetEnvironment) -> EnvironmentInfo,
    /// List a project's environments.
    ListEnvironments(ListEnvironments) -> Vec<EnvironmentInfo>,
    /// Fetch one deployment by ID.
    GetDeployment(GetDeployment) -> DeploymentInfo,
    /// Most recent deployment for a project, optionally per environment.
    GetLastDeployment(GetLastDeployment) -> DeploymentInfo,
    /// List a project's deployments.
    ListDeployments(ListDeployments) -> Vec<DeploymentInfo>,
    /// Call the platform's own HTTP API on behalf of a signed-in user.
    ApiCall(ApiCall) -> ApiCallResult,
}

/// Ties a request type to the response type it produces.
///
/// Implemented by the macro above, never by hand: the pairing only holds
/// because both enum variants come from one entry.
pub trait PlatformCall: Serialize + Send + Sync + 'static {
    /// What the platform returns for this call.
    type Output: Send;

    /// Wrap into the wire request enum.
    fn into_request(self) -> PlatformCallRequest;

    /// Extract this call's output from a reply.
    ///
    /// A mismatch means the platform answered a different call than the one
    /// asked, which can only happen if the two sides disagree about the
    /// protocol — so it is reported as such rather than silently discarded.
    fn output_from(response: PlatformCallResponse) -> Result<Self::Output, ProtocolMismatch>;
}

/// The platform replied to a different call than the one that was made.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error(
    "Platform channel replied with `{received}` to a `{expected}` request — \
     the plugin and the platform disagree about the channel protocol; \
     rebuild the plugin against this Temps version"
)]
pub struct ProtocolMismatch {
    pub expected: &'static str,
    pub received: &'static str,
}

impl ChannelResponse {
    /// Build a successful response.
    pub fn ok(id: u64, result: PlatformCallResponse) -> Self {
        Self {
            id,
            outcome: CallOutcome::Ok(Box::new(result)),
        }
    }

    /// Build an error response.
    pub fn err(id: u64, code: ChannelErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            outcome: CallOutcome::Err(ChannelError::new(code, message)),
        }
    }
}

// ── Well-known path ────────────────────────────────────────────────────

/// WebSocket endpoint path that plugins must serve for the platform channel.
pub const PLUGIN_CHANNEL_PATH: &str = "/_temps/channel";

// ── Response DTOs ──────────────────────────────────────────────────────
//
// These are the canonical types returned by platform channel methods.
// They are intentionally decoupled from the internal entity models and
// from the HTTP API response types — plugins get a stable, minimal
// contract that can evolve independently.

/// A project as returned by the platform channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub repo_name: String,
    pub repo_owner: String,
    pub main_branch: String,
    pub preset: String,
    pub source_type: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_deployment: Option<String>,
    pub enable_preview_environments: bool,
}

/// An environment as returned by the platform channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub slug: String,
    pub branch: Option<String>,
    pub is_preview: bool,
    pub current_deployment_id: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
}

/// A deployment as returned by the platform channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentInfo {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub state: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
    pub commit_author: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn project_info() -> ProjectInfo {
        ProjectInfo {
            id: 42,
            name: "demo".into(),
            slug: "demo".into(),
            repo_name: "demo".into(),
            repo_owner: "acme".into(),
            main_branch: "main".into(),
            preset: "node".into(),
            source_type: "github".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_deployment: None,
            enable_preview_environments: false,
        }
    }

    #[test]
    fn request_roundtrips_as_a_typed_call() {
        let msg = ChannelMessage::Request(ChannelRequest {
            id: 1,
            call: GetProject { project_id: 42 }.into_request(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        let deser: ChannelMessage = serde_json::from_str(&json).unwrap();
        match deser {
            ChannelMessage::Request(req) => {
                assert_eq!(req.id, 1);
                match req.call {
                    PlatformCallRequest::GetProject(p) => assert_eq!(p.project_id, 42),
                    other => panic!("wrong call: {}", other.method_name()),
                }
            }
            _ => panic!("Expected Request variant"),
        }
    }

    #[test]
    fn response_roundtrips_and_pairs_with_its_request() {
        let resp = ChannelResponse::ok(7, PlatformCallResponse::GetProject(project_info()));
        let json = serde_json::to_string(&ChannelMessage::Response(resp)).unwrap();
        let deser: ChannelMessage = serde_json::from_str(&json).unwrap();
        let ChannelMessage::Response(resp) = deser else {
            panic!("Expected Response variant")
        };
        let CallOutcome::Ok(payload) = resp.outcome else {
            panic!("Expected Ok outcome")
        };
        let project = <GetProject as PlatformCall>::output_from(*payload).unwrap();
        assert_eq!(project.id, 42);
    }

    /// The whole point of the pairing: answering the wrong call is caught,
    /// not silently returned as a default or a `None`.
    #[test]
    fn mismatched_reply_is_reported_as_a_protocol_disagreement() {
        let payload = PlatformCallResponse::ListProjects(vec![project_info()]);
        let err = <GetProject as PlatformCall>::output_from(payload).unwrap_err();
        assert_eq!(err.expected, "GetProject");
        assert_eq!(err.received, "ListProjects");
    }

    #[test]
    fn error_response_roundtrips() {
        let resp = ChannelResponse::err(2, ChannelErrorCode::NotFound, "Project 99 not found");
        let json = serde_json::to_string(&resp).unwrap();
        let deser: ChannelResponse = serde_json::from_str(&json).unwrap();
        let CallOutcome::Err(err) = deser.outcome else {
            panic!("Expected Err outcome")
        };
        assert_eq!(err.code, ChannelErrorCode::NotFound);
        assert!(err.message.contains("99"));
    }

    #[test]
    fn all_error_codes_roundtrip() {
        let codes = [
            ChannelErrorCode::MethodNotFound,
            ChannelErrorCode::InvalidParams,
            ChannelErrorCode::PermissionDenied,
            ChannelErrorCode::Unauthenticated,
            ChannelErrorCode::NotFound,
            ChannelErrorCode::Internal,
        ];
        for code in codes {
            let resp = ChannelResponse::err(0, code, "test");
            let json = serde_json::to_string(&resp).unwrap();
            let deser: ChannelResponse = serde_json::from_str(&json).unwrap();
            let CallOutcome::Err(err) = deser.outcome else {
                panic!("Expected Err outcome")
            };
            assert_eq!(err.code, code);
        }
    }

    #[test]
    fn json_body_roundtrips_a_typed_value() {
        let body = JsonBody::encode(&GetProject { project_id: 9 }).unwrap();
        let back: GetProject = body.decode().unwrap();
        assert_eq!(back.project_id, 9);
    }

    /// A decode failure must name the target type without echoing the body:
    /// request bodies carry credentials.
    #[test]
    fn json_body_decode_error_names_the_type_and_hides_the_payload() {
        let body = JsonBody::from_raw(r#"{"secret":"hunter2"}"#);
        let err = body.decode::<GetProject>().unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("GetProject"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn api_call_roundtrips_with_its_actor() {
        let call = ApiCall {
            method: HttpMethod::Post,
            path: "/projects".into(),
            query: None,
            body: Some(CallBody::Json(JsonBody::from_raw(r#"{"name":"demo"}"#))),
            actor: ActorToken::new("signed-token"),
        };
        let json = serde_json::to_string(&call.into_request()).unwrap();
        let deser: PlatformCallRequest = serde_json::from_str(&json).unwrap();
        match deser {
            PlatformCallRequest::ApiCall(c) => {
                assert_eq!(c.method, HttpMethod::Post);
                assert_eq!(c.actor.as_str(), "signed-token");
            }
            other => panic!("wrong call: {}", other.method_name()),
        }
    }

    /// Bytes must survive the JSON channel unchanged — including the ones
    /// that are not valid UTF-8, which is every real tarball.
    #[test]
    fn binary_payload_roundtrips_through_json() {
        let raw: Vec<u8> = (0u8..=255).collect();
        let call = ApiCall {
            method: HttpMethod::Post,
            path: "/projects/1/upload/static".into(),
            query: None,
            body: Some(CallBody::Multipart(vec![
                MultipartPart {
                    name: "file".into(),
                    filename: Some("bundle.tar.gz".into()),
                    content_type: Some("application/gzip".into()),
                    content: PartContent::Binary(BinaryPayload::new(raw.clone())),
                },
                MultipartPart {
                    name: "content_type".into(),
                    filename: None,
                    content_type: None,
                    content: PartContent::Text("application/gzip".into()),
                },
            ])),
            actor: ActorToken::new("signed-token"),
        };
        let json = serde_json::to_string(&call).unwrap();
        let back: ApiCall = serde_json::from_str(&json).unwrap();
        let Some(CallBody::Multipart(parts)) = back.body else {
            panic!("expected a multipart body")
        };
        assert_eq!(parts.len(), 2);
        match &parts[0].content {
            PartContent::Binary(b) => assert_eq!(b.as_bytes(), raw.as_slice()),
            other => panic!("expected binary, got {other:?}"),
        }
        assert_eq!(parts[0].filename.as_deref(), Some("bundle.tar.gz"));
        match &parts[1].content {
            PartContent::Text(t) => assert_eq!(t, "application/gzip"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A body's size must be knowable before it is sent, so an over-large
    /// upload is refused at the call site instead of after it has been
    /// buffered twice.
    #[test]
    fn payload_len_counts_content_not_framing() {
        let body = CallBody::Multipart(vec![
            MultipartPart {
                name: "file".into(),
                filename: Some("a.bin".into()),
                content_type: None,
                content: PartContent::Binary(BinaryPayload::new(vec![0u8; 1000])),
            },
            MultipartPart {
                name: "note".into(),
                filename: None,
                content_type: None,
                content: PartContent::Text("hello".into()),
            },
        ]);
        assert_eq!(body.payload_len(), 1005);
        // `{"project_id":1}`
        let json = CallBody::json(&GetProject { project_id: 1 }).unwrap();
        assert_eq!(json.payload_len(), 16);
    }

    /// Debug output is a length, never the payload: uploads are customer
    /// source code and a `{:?}` in a log line would leak it wholesale.
    #[test]
    fn binary_payload_debug_hides_the_bytes() {
        let payload = BinaryPayload::new(b"super-secret-source".to_vec());
        let rendered = format!("{payload:?}");
        assert!(rendered.contains("19 bytes"), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
    }

    #[test]
    fn only_get_is_non_mutating() {
        assert!(!HttpMethod::Get.is_mutating());
        for m in [
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
        ] {
            assert!(m.is_mutating(), "{m:?} must count as mutating");
        }
    }

    #[test]
    fn event_roundtrips() {
        let event = super::super::PluginEvent {
            id: "evt-1".into(),
            event_type: "deployment.succeeded".into(),
            timestamp: chrono::Utc::now(),
            project_id: Some(7),
            data: serde_json::json!({}),
        };
        let msg = ChannelMessage::Event(ChannelEvent { event });
        let json = serde_json::to_string(&msg).unwrap();
        let deser: ChannelMessage = serde_json::from_str(&json).unwrap();
        match deser {
            ChannelMessage::Event(evt) => {
                assert_eq!(evt.event.event_type, "deployment.succeeded");
            }
            _ => panic!("Expected Event variant"),
        }
    }
}
