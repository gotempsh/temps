//! Package.json type definitions
//!
//! Provides typed deserialization for package.json files

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Typed representation of a package.json file
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PackageJson {
    /// Package name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Package version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// NPM scripts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<HashMap<String, String>>,

    /// Package manager field (e.g., "pnpm@8.15.0")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,

    /// Runtime dependencies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<String, String>>,

    /// Development dependencies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_dependencies: Option<HashMap<String, String>>,

    /// Engine requirements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engines: Option<HashMap<String, String>>,

    /// Main entry point
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,

    /// Workspaces configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<String>>,
}

impl PackageJson {
    /// Parse package.json from string
    pub fn from_str(content: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(content)
    }

    /// Get dependency version by name
    /// Checks both dependencies and devDependencies
    pub fn get_dependency(&self, name: &str) -> Option<&str> {
        self.dependencies
            .as_ref()
            .and_then(|deps| deps.get(name))
            .or_else(|| {
                self.dev_dependencies
                    .as_ref()
                    .and_then(|deps| deps.get(name))
            })
            .map(|s| s.as_str())
    }

    /// Check if a dependency exists (in either dependencies or devDependencies)
    pub fn has_dependency(&self, name: &str) -> bool {
        self.get_dependency(name).is_some()
    }

    /// Check if any of the given dependency names exist
    pub fn has_any_dependency(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.has_dependency(name))
    }

    /// Get script by name
    pub fn get_script(&self, name: &str) -> Option<&str> {
        self.scripts
            .as_ref()
            .and_then(|scripts| scripts.get(name))
            .map(|s| s.as_str())
    }

    /// Check if a script exists
    pub fn has_script(&self, name: &str) -> bool {
        self.get_script(name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_json() {
        let content = r#"{
            "name": "test-app",
            "version": "1.0.0",
            "dependencies": {
                "react": "^18.0.0"
            },
            "devDependencies": {
                "typescript": "^5.0.0"
            }
        }"#;

        let pkg = PackageJson::from_str(content).unwrap();
        assert_eq!(pkg.name, Some("test-app".to_string()));
        assert_eq!(pkg.version, Some("1.0.0".to_string()));
        assert_eq!(pkg.get_dependency("react"), Some("^18.0.0"));
        assert_eq!(pkg.get_dependency("typescript"), Some("^5.0.0"));
    }

    #[test]
    fn test_has_dependency() {
        let content = r#"{
            "dependencies": {
                "react": "^18.0.0"
            },
            "devDependencies": {
                "typescript": "^5.0.0"
            }
        }"#;

        let pkg = PackageJson::from_str(content).unwrap();
        assert!(pkg.has_dependency("react"));
        assert!(pkg.has_dependency("typescript"));
        assert!(!pkg.has_dependency("vue"));
    }

    #[test]
    fn test_package_manager_field() {
        let content = r#"{
            "packageManager": "pnpm@8.15.0"
        }"#;

        let pkg = PackageJson::from_str(content).unwrap();
        assert_eq!(pkg.package_manager, Some("pnpm@8.15.0".to_string()));
    }

    #[test]
    fn test_scripts() {
        let content = r#"{
            "scripts": {
                "build": "tsc",
                "dev": "vite"
            }
        }"#;

        let pkg = PackageJson::from_str(content).unwrap();
        assert_eq!(pkg.get_script("build"), Some("tsc"));
        assert_eq!(pkg.get_script("dev"), Some("vite"));
        assert!(pkg.has_script("build"));
        assert!(!pkg.has_script("test"));
    }
}
