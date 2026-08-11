//! External plugin manifest types.
//!
//! These types define the contract between Temps and external plugin binaries.
//! They are the canonical definitions — the `temps-plugin-sdk` crate re-exports them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where the plugin's nav entry appears in the Temps UI sidebar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NavSection {
    /// Main platform navigation (Dashboard, Projects, Storage, Domains, Monitoring)
    Platform,
    /// Settings/admin section (Settings, Users, Backups, etc.)
    Settings,
    /// Inside project detail view (per-project feature)
    Project,
}

/// A navigation entry that the plugin contributes to the Temps UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NavEntry {
    /// Display label in the sidebar
    pub label: String,
    /// Lucide icon name (e.g., "puzzle", "database", "activity")
    pub icon: String,
    /// Which sidebar section this entry belongs to
    pub section: NavSection,
    /// Client-side route path (e.g., "/my-plugin")
    pub path: String,
    /// Sort order within the section (lower = higher in list)
    pub order: u32,
}

/// Describes the plugin's embedded UI bundle.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UiManifest {
    /// JavaScript entry point filename relative to the bundle root
    pub entry_js: String,
    /// CSS files to load
    #[serde(default)]
    pub css: Vec<String>,
    /// Client-side routes the plugin handles
    #[serde(default)]
    pub routes: Vec<UiRoute>,
}

/// A client-side route provided by the plugin UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UiRoute {
    /// Route path pattern (e.g., "/my-plugin", "/my-plugin/:id")
    pub path: String,
    /// Page title for breadcrumbs
    pub title: String,
}

/// The complete plugin manifest — the handshake contract.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PluginManifest {
    /// Unique plugin identifier (kebab-case, e.g., "backup-manager")
    pub name: String,
    /// SemVer version string
    pub version: String,
    /// Human-readable display name
    #[serde(default)]
    pub display_name: Option<String>,
    /// Short description of what the plugin does
    #[serde(default)]
    pub description: Option<String>,
    /// Navigation entries for the UI sidebar
    #[serde(default)]
    pub nav: Vec<NavEntry>,
    /// UI bundle manifest (if the plugin has a UI)
    #[serde(default)]
    pub ui: Option<UiManifest>,
    /// Whether the plugin needs database access
    #[serde(default)]
    pub requires_db: bool,
    /// Whether the plugin may read the platform's host data root.
    ///
    /// This is a highly privileged capability: the directory can contain
    /// encryption keys and instance-owned state. It is independent of direct
    /// database access and defaults to `false`.
    #[serde(default)]
    pub requires_host_data_access: bool,
    /// Health check endpoint path (relative to plugin root)
    #[serde(default = "default_health_path")]
    pub health_path: String,
    /// Suppress the console's own header strip above this plugin's UI.
    ///
    /// The console normally renders the plugin's icon, display name and
    /// version above the iframe. For a plugin whose UI is a full working
    /// surface with its own header, that is a second title bar competing
    /// for the same vertical space — and vertical space is exactly what a
    /// dense full-page layout has none of. Opt out and the frame gets the
    /// full height.
    ///
    /// The nav entry still names the plugin, so nothing becomes
    /// unidentifiable by setting this.
    #[serde(default)]
    pub hide_header: bool,
    /// Routes this plugin authenticates itself, which the platform's proxy
    /// therefore does not gate.
    ///
    /// Every other proxied route requires an authenticated caller before it
    /// reaches the plugin. That is right for anything a signed-in user
    /// drives, and wrong for the endpoints a plugin exposes to clients that
    /// hold no platform session — an agent in a sandbox presenting a
    /// capability token, a share link opened by someone with no account.
    /// This is the external-plugin counterpart of the in-process
    /// `configure_public_routes`.
    ///
    /// Paths are relative to the plugin's mount point and match by prefix,
    /// so `/webhooks/incoming` covers everything beneath it. A listed route is
    /// reachable by anyone who can reach the instance: it **must** check its
    /// own credential. Listing a route that does not is an open door.
    #[serde(default)]
    pub public_paths: Vec<String>,
    /// What this plugin may do with the platform's own API over the channel.
    ///
    /// Empty by default, and empty means read-only channel queries and
    /// nothing else: a plugin that never asks cannot deploy, cannot create
    /// projects, and cannot provision databases. Declaring a capability is
    /// not by itself permission to act — every call still runs the real
    /// handler's `permission_guard!` as the user the plugin is acting for,
    /// so a capability can only ever narrow what that user could already do
    /// through the console.
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    /// Platform event types the plugin subscribes to.
    ///
    /// When specified, Temps will POST matching events to the plugin's
    /// `/_events` endpoint. Uses dot-notation event names matching the
    /// webhook event types (e.g., "deployment.succeeded", "project.created").
    ///
    /// Available events:
    /// - `deployment.created`, `deployment.succeeded`, `deployment.failed`,
    ///   `deployment.cancelled`, `deployment.ready`
    /// - `project.created`, `project.deleted`
    /// - `domain.created`, `domain.provisioned`
    #[serde(default)]
    pub events: Vec<String>,
}

fn default_health_path() -> String {
    "/health".to_string()
}

/// What a plugin is allowed to do with the platform API over the channel.
///
/// Coarse on purpose. Fine-grained authorization already exists in the
/// permission system and is enforced per request against the acting user;
/// this exists so an operator installing a binary can see at a glance
/// whether it intends to *write* at all, without reading its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    /// Read platform resources through the API (`GET`).
    ApiRead,
    /// Create, change or delete platform resources through the API.
    ApiWrite,
}

impl PluginCapability {
    /// The capability a given verb requires.
    pub fn for_method(method: super::channel::HttpMethod) -> Self {
        if method.is_mutating() {
            Self::ApiWrite
        } else {
            Self::ApiRead
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiRead => "api_read",
            Self::ApiWrite => "api_write",
        }
    }
}

/// Builder for constructing a PluginManifest.
pub struct PluginManifestBuilder {
    manifest: PluginManifest,
}

impl PluginManifest {
    pub fn builder(name: impl Into<String>, version: impl Into<String>) -> PluginManifestBuilder {
        PluginManifestBuilder {
            manifest: PluginManifest {
                name: name.into(),
                version: version.into(),
                display_name: None,
                description: None,
                nav: Vec::new(),
                ui: None,
                requires_db: false,
                requires_host_data_access: false,
                health_path: "/health".to_string(),
                // Empty by default: a plugin opts routes out of platform
                // auth explicitly, never by omission.
                public_paths: Vec::new(),
                capabilities: Vec::new(),
                hide_header: false,
                events: Vec::new(),
            },
        }
    }
}

impl PluginManifestBuilder {
    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.manifest.display_name = Some(name.into());
        self
    }

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.manifest.description = Some(desc.into());
        self
    }

    pub fn nav(mut self, entry: NavEntry) -> Self {
        self.manifest.nav.push(entry);
        self
    }

    pub fn ui(mut self, ui_manifest: UiManifest) -> Self {
        self.manifest.ui = Some(ui_manifest);
        self
    }

    pub fn requires_db(mut self, requires: bool) -> Self {
        self.manifest.requires_db = requires;
        self
    }

    /// Request access to the platform's host data root.
    ///
    /// This exposes instance key material and other host-owned state. Plugins
    /// should use their private data directory unless they explicitly need to
    /// operate on platform-managed files.
    pub fn requires_host_data_access(mut self, requires: bool) -> Self {
        self.manifest.requires_host_data_access = requires;
        self
    }

    pub fn health_path(mut self, path: impl Into<String>) -> Self {
        self.manifest.health_path = path.into();
        self
    }

    /// Declare a capability this plugin needs for its channel API calls.
    ///
    /// Without the matching capability a call is refused before it reaches
    /// the router, and the error names what was missing so a plugin author
    /// is not left guessing.
    pub fn capability(mut self, capability: PluginCapability) -> Self {
        if !self.manifest.capabilities.contains(&capability) {
            self.manifest.capabilities.push(capability);
        }
        self
    }

    /// Hide the console's header strip above this plugin's UI.
    ///
    /// See [`PluginManifest::hide_header`]. Use this for plugins that render
    /// their own full-page header.
    pub fn hide_header(mut self, hide: bool) -> Self {
        self.manifest.hide_header = hide;
        self
    }

    /// Declare a route prefix the platform should not gate, because the
    /// plugin authenticates it itself. See [`PluginManifest::public_paths`] —
    /// anything listed here is reachable unauthenticated.
    pub fn public_path(mut self, path: impl Into<String>) -> Self {
        self.manifest.public_paths.push(path.into());
        self
    }

    /// Subscribe to a platform event (e.g., "deployment.succeeded").
    pub fn event(mut self, event_type: impl Into<String>) -> Self {
        self.manifest.events.push(event_type.into());
        self
    }

    /// Subscribe to multiple platform events at once.
    pub fn events(mut self, event_types: Vec<String>) -> Self {
        self.manifest.events.extend(event_types);
        self
    }

    pub fn build(self) -> PluginManifest {
        self.manifest
    }
}

/// Message sent from plugin to Temps after the server is ready.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginReady {
    pub ready: bool,
    pub has_ui: bool,
    /// External-plugin protocol version implemented by the running SDK.
    pub protocol_version: u32,
    /// Optional OpenAPI schema (serialized JSON) for the plugin's endpoints.
    ///
    /// When present, Temps merges this into the unified API documentation
    /// with paths prefixed under `/x/{plugin_name}/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openapi: Option<serde_json::Value>,
}

/// First message emitted by a staged plugin process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHello {
    pub protocol_version: u32,
    pub manifest: Box<PluginManifest>,
}

/// Configuration sent to the same child after Temps reads its manifest.
///
/// Database and host-data fields reflect the manifest's declared needs. They
/// are operational routing/disclosure controls for trusted installed code,
/// not a sandbox boundary.
#[derive(Clone, Serialize, Deserialize)]
pub struct PluginLaunchConfig {
    pub protocol_version: u32,
    pub auth_secret: String,
    pub database_url: Option<String>,
    pub host_data_dir: Option<String>,
}

/// Current process-launch and internal-channel protocol version.
pub const EXTERNAL_PLUGIN_PROTOCOL_VERSION: u32 = 2;

/// Handshake envelope: tagged union for messages from plugin to Temps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HandshakeMessage {
    #[serde(rename = "hello")]
    Hello(PluginHello),
    /// Legacy handshake retained only so the manager can return an actionable
    /// incompatibility error instead of an opaque JSON parse failure.
    #[serde(rename = "manifest")]
    Manifest(Box<PluginManifest>),
    #[serde(rename = "ready")]
    Ready(PluginReady),
}

/// Well-known endpoint path where Temps delivers events to plugins.
///
/// Temps POSTs a JSON [`PluginEvent`] to this path on the plugin's Unix socket
/// whenever a subscribed event occurs.
pub const PLUGIN_EVENTS_PATH: &str = "/_events";

/// An event delivered from Temps to a plugin.
///
/// This is the JSON body POSTed to `/_events` on the plugin's Unix socket.
/// The structure mirrors the webhook payload format for consistency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEvent {
    /// Unique event ID (UUID)
    pub id: String,
    /// Event type in dot-notation (e.g., "deployment.succeeded")
    pub event_type: String,
    /// ISO 8601 timestamp when the event occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Associated project ID, if applicable
    pub project_id: Option<i32>,
    /// Event-specific payload as a JSON value.
    ///
    /// The structure depends on the event type:
    /// - `deployment.*` events include deployment details (id, branch, commit, status, etc.)
    /// - `project.*` events include project details (id, name, slug, repo_url)
    /// - `domain.*` events include domain details (id, name, project, ssl_status)
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_builder() {
        let manifest = PluginManifest::builder("test-plugin", "0.1.0")
            .display_name("Test Plugin")
            .description("A test plugin")
            .nav(NavEntry {
                label: "Test".into(),
                icon: "puzzle".into(),
                section: NavSection::Platform,
                path: "/test".into(),
                order: 50,
            })
            .requires_db(true)
            .build();

        assert_eq!(manifest.name, "test-plugin");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.display_name, Some("Test Plugin".to_string()));
        assert_eq!(manifest.nav.len(), 1);
        assert!(manifest.requires_db);
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let manifest = PluginManifest::builder("my-plugin", "1.0.0")
            .display_name("My Plugin")
            .nav(NavEntry {
                label: "My Feature".into(),
                icon: "star".into(),
                section: NavSection::Settings,
                path: "/my-feature".into(),
                order: 10,
            })
            .build();

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "my-plugin");
        assert_eq!(deserialized.nav.len(), 1);
        assert_eq!(deserialized.nav[0].section, NavSection::Settings);
        assert!(!deserialized.requires_db);
        assert!(!deserialized.requires_host_data_access);
    }

    #[test]
    fn host_data_access_is_separate_and_explicit() {
        let manifest = PluginManifest::builder("privileged-plugin", "1.0.0")
            .requires_host_data_access(true)
            .build();

        assert!(manifest.requires_host_data_access);
        assert!(!manifest.requires_db);
    }

    #[test]
    fn test_handshake_message_serialization() {
        let hello = HandshakeMessage::Hello(PluginHello {
            protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            manifest: Box::new(PluginManifest::builder("test", "0.1.0").build()),
        });
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"protocol_version\":2"));

        // Kept parseable so a legacy child gets an upgrade instruction from
        // the manager instead of an opaque deserialization failure.
        let msg =
            HandshakeMessage::Manifest(Box::new(PluginManifest::builder("test", "0.1.0").build()));
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"manifest\""));

        let ready_msg = HandshakeMessage::Ready(PluginReady {
            ready: true,
            has_ui: false,
            protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            openapi: None,
        });
        let json = serde_json::to_string(&ready_msg).unwrap();
        assert!(json.contains("\"type\":\"ready\""));

        let launch = PluginLaunchConfig {
            protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
            auth_secret: "secret".to_string(),
            database_url: None,
            host_data_dir: None,
        };
        let launch_json = serde_json::to_string(&launch).unwrap();
        let decoded: PluginLaunchConfig = serde_json::from_str(&launch_json).unwrap();
        assert_eq!(decoded.protocol_version, EXTERNAL_PLUGIN_PROTOCOL_VERSION);
        assert_eq!(decoded.auth_secret, "secret");
    }

    #[test]
    fn test_manifest_builder_event() {
        let manifest = PluginManifest::builder("seo-plugin", "1.0.0")
            .event("deployment.succeeded")
            .event("deployment.ready")
            .build();

        assert_eq!(manifest.events.len(), 2);
        assert_eq!(manifest.events[0], "deployment.succeeded");
        assert_eq!(manifest.events[1], "deployment.ready");
    }

    #[test]
    fn test_manifest_builder_events_batch() {
        let manifest = PluginManifest::builder("audit-plugin", "1.0.0")
            .events(vec![
                "project.created".to_string(),
                "project.deleted".to_string(),
                "deployment.*".to_string(),
            ])
            .build();

        assert_eq!(manifest.events.len(), 3);
        assert!(manifest.events.contains(&"deployment.*".to_string()));
    }

    #[test]
    fn test_manifest_events_default_empty() {
        let manifest = PluginManifest::builder("no-events", "1.0.0").build();
        assert!(manifest.events.is_empty());
    }

    #[test]
    fn test_manifest_events_serialization_roundtrip() {
        let manifest = PluginManifest::builder("test", "1.0.0")
            .event("deployment.succeeded")
            .event("project.created")
            .build();

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("deployment.succeeded"));
        assert!(json.contains("project.created"));

        let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.events.len(), 2);
        assert_eq!(deserialized.events[0], "deployment.succeeded");
        assert_eq!(deserialized.events[1], "project.created");
    }

    #[test]
    fn test_manifest_events_deserialize_missing_field() {
        // Old manifests without "events" field should deserialize with empty vec
        let json = r#"{"name":"old-plugin","version":"1.0.0"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.events.is_empty());
    }

    #[test]
    fn test_plugin_event_serialization_roundtrip() {
        let event = PluginEvent {
            id: "test-uuid".to_string(),
            event_type: "deployment.succeeded".to_string(),
            timestamp: chrono::Utc::now(),
            project_id: Some(42),
            data: serde_json::json!({
                "deployment_id": 100,
                "url": "https://example.com",
            }),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: PluginEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "test-uuid");
        assert_eq!(deserialized.event_type, "deployment.succeeded");
        assert_eq!(deserialized.project_id, Some(42));
        assert_eq!(deserialized.data["deployment_id"], 100);
    }

    #[test]
    fn test_plugin_events_path_constant() {
        assert_eq!(PLUGIN_EVENTS_PATH, "/_events");
    }
}
