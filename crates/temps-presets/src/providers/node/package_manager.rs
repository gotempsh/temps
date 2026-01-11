//! Package manager detection for Node.js projects
//!
//! Detects package manager based on (in priority order):
//! 1. packageManager field in package.json
//! 2. Lock files (pnpm-lock.yaml, bun.lockb, yarn.lock, package-lock.json)
//! 3. Configuration files (.yarnrc.yml, .yarnrc.yaml)
//! 4. engines field in package.json

use crate::providers::app::App;
use crate::providers::package_json::PackageJson;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn1,
    YarnBerry,
    Bun,
}

impl PackageManager {
    /// Detect package manager from app context
    /// Priority: packageManager field → lock files → config files → engines field → default (npm)
    pub fn detect(app: &App) -> Self {
        // 1. Check packageManager field in package.json
        if let Some(pm) = Self::from_package_json_field(app) {
            return pm;
        }

        // 2. Check lock files
        if app.includes_file("pnpm-lock.yaml") {
            return PackageManager::Pnpm;
        }
        if app.includes_file("bun.lockb") || app.includes_file("bun.lock") {
            return PackageManager::Bun;
        }
        if app.includes_file(".yarnrc.yml") || app.includes_file(".yarnrc.yaml") {
            return PackageManager::YarnBerry;
        }
        if app.includes_file("yarn.lock") {
            return PackageManager::Yarn1;
        }

        // 3. Check engines field as last resort
        if let Some(pm) = Self::from_engines_field(app) {
            return pm;
        }

        // 4. Default to npm
        PackageManager::Npm
    }

    /// Extract package manager from packageManager field in package.json
    fn from_package_json_field(app: &App) -> Option<Self> {
        let content = app.read_file("package.json").ok()?;
        let package_json = PackageJson::parse(&content).ok()?;
        let package_manager = package_json.package_manager.as_ref()?;

        // Format: "pnpm@8.15.0" or "yarn@4.0.2"
        let parts: Vec<&str> = package_manager.split('@').collect();
        if parts.is_empty() {
            return None;
        }

        let pm_name = parts[0];
        let pm_version = parts.get(1);

        match pm_name {
            "npm" => Some(PackageManager::Npm),
            "pnpm" => Some(PackageManager::Pnpm),
            "bun" => Some(PackageManager::Bun),
            "yarn" => {
                // Determine yarn version
                if let Some(version) = pm_version {
                    let major = version.split('.').next()?;
                    if major == "1" {
                        Some(PackageManager::Yarn1)
                    } else {
                        Some(PackageManager::YarnBerry)
                    }
                } else {
                    // No version specified, default to Yarn 1
                    Some(PackageManager::Yarn1)
                }
            }
            _ => None,
        }
    }

    /// Extract package manager from engines field in package.json
    fn from_engines_field(app: &App) -> Option<Self> {
        let content = app.read_file("package.json").ok()?;
        let package_json = PackageJson::parse(&content).ok()?;
        let engines = package_json.engines.as_ref()?;

        // Check in order: pnpm, bun, yarn, npm
        if let Some(pnpm) = engines.get("pnpm") {
            if !pnpm.trim().is_empty() {
                return Some(PackageManager::Pnpm);
            }
        }

        if let Some(bun) = engines.get("bun") {
            if !bun.trim().is_empty() {
                return Some(PackageManager::Bun);
            }
        }

        if let Some(yarn) = engines.get("yarn") {
            if !yarn.trim().is_empty() {
                // Determine version from engines
                let major = yarn.trim().split('.').next()?;
                return if major == "1" {
                    Some(PackageManager::Yarn1)
                } else {
                    Some(PackageManager::YarnBerry)
                };
            }
        }

        None
    }

    /// Check if package manager uses Corepack
    pub fn uses_corepack(self, app: &App) -> bool {
        // Corepack is used when packageManager field exists and it's not Bun
        if self == PackageManager::Bun {
            return false;
        }

        if let Ok(content) = app.read_file("package.json") {
            if let Ok(package_json) = PackageJson::parse(&content) {
                return package_json.package_manager.is_some();
            }
        }

        false
    }

    /// Get install command for this package manager
    pub fn install_command(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm ci",
            PackageManager::Pnpm => "pnpm install --frozen-lockfile",
            PackageManager::Yarn1 => "yarn install --frozen-lockfile",
            PackageManager::YarnBerry => "yarn install --immutable",
            PackageManager::Bun => "bun install",
        }
    }

    /// Get build command prefix for this package manager
    pub fn build_command(self, script: &str) -> String {
        match self {
            PackageManager::Npm => format!("npm run {}", script),
            PackageManager::Pnpm => format!("pnpm run {}", script),
            PackageManager::Yarn1 | PackageManager::YarnBerry => format!("yarn {}", script),
            PackageManager::Bun => format!("bun run {}", script),
        }
    }

    /// Get start command prefix for this package manager
    pub fn start_command(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm start",
            PackageManager::Pnpm => "pnpm start",
            PackageManager::Yarn1 | PackageManager::YarnBerry => "yarn start",
            PackageManager::Bun => "bun start",
        }
    }

    /// Get run command for executing a script file
    pub fn run_script_command(self, script_path: &str) -> String {
        match self {
            PackageManager::Npm => format!("node {}", script_path),
            PackageManager::Pnpm => format!("node {}", script_path),
            PackageManager::Yarn1 | PackageManager::YarnBerry => format!("node {}", script_path),
            PackageManager::Bun => format!("bun {}", script_path),
        }
    }

    /// Get the node_modules install folder
    pub fn install_folder(self) -> &'static str {
        match self {
            PackageManager::YarnBerry => {
                // Yarn Berry uses .yarn/cache with OnP or node_modules with nodeLinker
                // For simplicity, default to node_modules
                "node_modules"
            }
            _ => "node_modules",
        }
    }

    /// Get package manager name as string
    pub fn name(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn1 => "yarn",
            PackageManager::YarnBerry => "yarn",
            PackageManager::Bun => "bun",
        }
    }

    /// Get base Docker image for this package manager
    /// Uses the detected Node.js version from engines field, or defaults to LTS (22)
    pub fn base_image(self) -> &'static str {
        match self {
            PackageManager::Bun => "oven/bun:1.2",
            PackageManager::Npm | PackageManager::Pnpm | PackageManager::Yarn1 | PackageManager::YarnBerry => "node:22-alpine",
        }
    }

    /// Get base Docker image with specific Node.js version from app
    /// Detects Node.js version from engines field in package.json
    /// Always uses Node.js image (even for Bun - Bun will be installed on top)
    pub fn base_image_for_app(self, app: &App) -> String {
        let node_version = Self::detect_node_version(app);
        format!("node:{}-alpine", node_version)
    }

    /// Check if this package manager requires installation (Bun needs to be installed on Node image)
    pub fn needs_installation(self) -> bool {
        matches!(self, PackageManager::Bun)
    }

    /// Get installation commands for the package manager itself
    /// Returns commands to install Bun on a Node.js image using official install script
    pub fn installation_commands(self) -> Vec<String> {
        match self {
            PackageManager::Bun => vec![
                "curl -fsSL https://bun.sh/install | bash".to_string(),
            ],
            _ => vec![],
        }
    }

    /// Detect Node.js version from package.json engines field
    /// Returns LTS version (22) as default
    pub fn detect_node_version(app: &App) -> &'static str {
        const DEFAULT_NODE_VERSION: &str = "22"; // LTS

        let content = match app.read_file("package.json") {
            Ok(c) => c,
            Err(_) => return DEFAULT_NODE_VERSION,
        };

        let package_json = match PackageJson::parse(&content) {
            Ok(pj) => pj,
            Err(_) => return DEFAULT_NODE_VERSION,
        };

        let engines = match package_json.engines {
            Some(e) => e,
            None => return DEFAULT_NODE_VERSION,
        };

        let version_str = match engines.get("node") {
            Some(v) => v,
            None => return DEFAULT_NODE_VERSION,
        };

        // Parse version string (handles ">=18", "^18.0.0", "18.x", etc.)
        Self::parse_node_version(version_str)
    }

    /// Parse Node.js version from engines field
    /// Extracts major version number and maps to supported versions
    fn parse_node_version(version_str: &str) -> &'static str {
        const DEFAULT_NODE_VERSION: &str = "22"; // LTS

        // Remove common prefixes and suffixes
        let cleaned = version_str
            .trim()
            .trim_start_matches(">=")
            .trim_start_matches("^")
            .trim_start_matches("~")
            .trim_start_matches('>')
            .trim_start_matches('=')
            .trim();

        // Extract major version
        let major = cleaned
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok());

        match major {
            Some(18) => "18",
            Some(20) => "20",
            Some(22) => "22",
            Some(v) if v >= 22 => "22", // Use latest LTS for newer versions
            Some(v) if v >= 20 => "20",
            Some(v) if v >= 18 => "18",
            _ => DEFAULT_NODE_VERSION, // Default to LTS for invalid or old versions
        }
    }

    /// Get package manager version requirement for Corepack
    pub fn corepack_version(self, app: &App) -> Option<String> {
        let content = app.read_file("package.json").ok()?;
        let package_json = PackageJson::parse(&content).ok()?;

        // Return the packageManager field value (format: "pnpm@8.15.0" or "yarn@4.0.2")
        package_json.package_manager.clone()
    }
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn create_test_app(files: HashMap<String, String>) -> App {
        App::from_tree(PathBuf::from("/test"), files)
    }

    #[test]
    fn test_detect_npm_default() {
        let files = HashMap::new();
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Npm);
    }

    #[test]
    fn test_detect_pnpm_from_lock() {
        let mut files = HashMap::new();
        files.insert("pnpm-lock.yaml".to_string(), "".to_string());
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Pnpm);
    }

    #[test]
    fn test_detect_bun_from_lock() {
        let mut files = HashMap::new();
        files.insert("bun.lockb".to_string(), "".to_string());
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Bun);
    }

    #[test]
    fn test_detect_yarn1_from_lock() {
        let mut files = HashMap::new();
        files.insert("yarn.lock".to_string(), "".to_string());
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Yarn1);
    }

    #[test]
    fn test_detect_yarn_berry_from_config() {
        let mut files = HashMap::new();
        files.insert(".yarnrc.yml".to_string(), "".to_string());
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::YarnBerry);
    }

    #[test]
    fn test_detect_from_package_manager_field() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "pnpm@8.15.0"}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Pnpm);
    }

    #[test]
    fn test_detect_yarn1_from_package_manager_field() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "yarn@1.22.0"}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Yarn1);
    }

    #[test]
    fn test_detect_yarn_berry_from_package_manager_field() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "yarn@4.0.2"}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::YarnBerry);
    }

    #[test]
    fn test_detect_from_engines_field() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"pnpm": ">=8.0.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Pnpm);
    }

    #[test]
    fn test_package_manager_field_takes_priority() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "bun@1.0.0"}"#.to_string(),
        );
        // Even with pnpm lock file, packageManager field takes priority
        files.insert("pnpm-lock.yaml".to_string(), "".to_string());
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect(&app), PackageManager::Bun);
    }

    #[test]
    fn test_uses_corepack() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "pnpm@8.15.0"}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::detect(&app);
        assert!(pm.uses_corepack(&app));
    }

    #[test]
    fn test_bun_does_not_use_corepack() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "bun@1.0.0"}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::detect(&app);
        assert!(!pm.uses_corepack(&app));
    }

    #[test]
    fn test_install_commands() {
        assert_eq!(PackageManager::Npm.install_command(), "npm ci");
        assert_eq!(
            PackageManager::Pnpm.install_command(),
            "pnpm install --frozen-lockfile"
        );
        assert_eq!(
            PackageManager::Yarn1.install_command(),
            "yarn install --frozen-lockfile"
        );
        assert_eq!(
            PackageManager::YarnBerry.install_command(),
            "yarn install --immutable"
        );
        assert_eq!(PackageManager::Bun.install_command(), "bun install");
    }

    #[test]
    fn test_build_commands() {
        assert_eq!(
            PackageManager::Npm.build_command("build"),
            "npm run build"
        );
        assert_eq!(
            PackageManager::Pnpm.build_command("build"),
            "pnpm run build"
        );
        assert_eq!(PackageManager::Yarn1.build_command("build"), "yarn build");
        assert_eq!(
            PackageManager::YarnBerry.build_command("build"),
            "yarn build"
        );
        assert_eq!(PackageManager::Bun.build_command("build"), "bun run build");
    }

    #[test]
    fn test_corepack_version() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"packageManager": "pnpm@8.15.0"}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::detect(&app);
        assert_eq!(pm.corepack_version(&app), Some("pnpm@8.15.0".to_string()));
    }

    #[test]
    fn test_detect_node_version_default_lts() {
        // No package.json - should default to LTS (22)
        let files = HashMap::new();
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22");
    }

    #[test]
    fn test_detect_node_version_no_engines() {
        // package.json without engines field - should default to LTS
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"name": "test-app"}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22");
    }

    #[test]
    fn test_detect_node_version_no_node_engine() {
        // engines field without node - should default to LTS
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"npm": ">=8.0.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22");
    }

    #[test]
    fn test_detect_node_version_18() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "18"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "18");
    }

    #[test]
    fn test_detect_node_version_20() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "20"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "20");
    }

    #[test]
    fn test_detect_node_version_22() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "22"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22");
    }

    #[test]
    fn test_detect_node_version_with_gte_operator() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": ">=18.0.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "18");
    }

    #[test]
    fn test_detect_node_version_with_caret() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "^20.5.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "20");
    }

    #[test]
    fn test_detect_node_version_with_tilde() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "~18.12.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "18");
    }

    #[test]
    fn test_detect_node_version_with_x_notation() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "20.x"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "20");
    }

    #[test]
    fn test_detect_node_version_future_version() {
        // Version 24 doesn't exist yet - should map to latest LTS (22)
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": ">=24.0.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22");
    }

    #[test]
    fn test_detect_node_version_old_version() {
        // Old version like 16 - should map to 18 (minimum supported)
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "16"}}"#.to_string(),
        );
        let app = create_test_app(files);
        assert_eq!(PackageManager::detect_node_version(&app), "22"); // Default LTS for old versions
    }

    #[test]
    fn test_parse_node_version_exact() {
        assert_eq!(PackageManager::parse_node_version("18"), "18");
        assert_eq!(PackageManager::parse_node_version("20"), "20");
        assert_eq!(PackageManager::parse_node_version("22"), "22");
    }

    #[test]
    fn test_parse_node_version_with_operators() {
        assert_eq!(PackageManager::parse_node_version(">=18"), "18");
        assert_eq!(PackageManager::parse_node_version("^20.5.0"), "20");
        assert_eq!(PackageManager::parse_node_version("~18.12.0"), "18");
        assert_eq!(PackageManager::parse_node_version(">18"), "18");
        assert_eq!(PackageManager::parse_node_version("=20"), "20");
    }

    #[test]
    fn test_parse_node_version_with_patch_versions() {
        assert_eq!(PackageManager::parse_node_version("18.0.0"), "18");
        assert_eq!(PackageManager::parse_node_version("20.11.1"), "20");
        assert_eq!(PackageManager::parse_node_version("22.3.0"), "22");
    }

    #[test]
    fn test_parse_node_version_invalid() {
        // Invalid version strings should return default LTS
        assert_eq!(PackageManager::parse_node_version("invalid"), "22");
        assert_eq!(PackageManager::parse_node_version(""), "22");
        assert_eq!(PackageManager::parse_node_version("abc"), "22");
    }

    #[test]
    fn test_base_image_for_app_with_node_18() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "18"}}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::Npm;
        assert_eq!(pm.base_image_for_app(&app), "node:18-alpine");
    }

    #[test]
    fn test_base_image_for_app_with_node_20() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": ">=20.0.0"}}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::Pnpm;
        assert_eq!(pm.base_image_for_app(&app), "node:20-alpine");
    }

    #[test]
    fn test_base_image_for_app_default_lts() {
        let files = HashMap::new();
        let app = create_test_app(files);
        let pm = PackageManager::Yarn1;
        assert_eq!(pm.base_image_for_app(&app), "node:22-alpine");
    }

    #[test]
    fn test_base_image_for_app_bun_uses_node_image() {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            r#"{"engines": {"node": "18"}}"#.to_string(),
        );
        let app = create_test_app(files);
        let pm = PackageManager::Bun;
        // Bun now uses Node.js image (Bun will be installed on top)
        assert_eq!(pm.base_image_for_app(&app), "node:18-alpine");
    }

    #[test]
    fn test_bun_needs_installation() {
        assert!(PackageManager::Bun.needs_installation());
        assert!(!PackageManager::Npm.needs_installation());
        assert!(!PackageManager::Pnpm.needs_installation());
        assert!(!PackageManager::Yarn1.needs_installation());
        assert!(!PackageManager::YarnBerry.needs_installation());
    }

    #[test]
    fn test_bun_installation_commands() {
        let commands = PackageManager::Bun.installation_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0], "curl -fsSL https://bun.sh/install | bash");

        // Other package managers don't need installation
        assert!(PackageManager::Npm.installation_commands().is_empty());
        assert!(PackageManager::Pnpm.installation_commands().is_empty());
        assert!(PackageManager::Yarn1.installation_commands().is_empty());
        assert!(PackageManager::YarnBerry.installation_commands().is_empty());
    }
}
