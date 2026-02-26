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
    #[serde(default = "default_true")]
    pub requires_db: bool,
    /// Health check endpoint path (relative to plugin root)
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

fn default_true() -> bool {
    true
}

fn default_health_path() -> String {
    "/health".to_string()
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
                requires_db: true,
                health_path: "/health".to_string(),
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

    pub fn health_path(mut self, path: impl Into<String>) -> Self {
        self.manifest.health_path = path.into();
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
}

/// Handshake envelope: tagged union for messages from plugin to Temps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HandshakeMessage {
    #[serde(rename = "manifest")]
    Manifest(Box<PluginManifest>),
    #[serde(rename = "ready")]
    Ready(PluginReady),
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
    }

    #[test]
    fn test_handshake_message_serialization() {
        let msg =
            HandshakeMessage::Manifest(Box::new(PluginManifest::builder("test", "0.1.0").build()));
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"manifest\""));

        let ready_msg = HandshakeMessage::Ready(PluginReady {
            ready: true,
            has_ui: false,
        });
        let json = serde_json::to_string(&ready_msg).unwrap();
        assert!(json.contains("\"type\":\"ready\""));
    }
}
