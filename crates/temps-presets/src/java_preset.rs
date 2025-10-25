//! Java preset implementation using Nixpacks
//!
//! This preset detects Java projects (pom.xml, build.gradle, build.gradle.kts)
//! and uses Nixpacks for building.

use crate::{DockerfileConfig, DockerfileWithArgs, NixpacksPreset, NixpacksProvider, Preset, ProjectType};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Java preset - delegates to Nixpacks with Java provider
#[derive(Debug, Clone, Copy)]
pub struct JavaPreset;

impl JavaPreset {
    /// Create a new Java preset instance
    pub fn new() -> Self {
        Self
    }
}

impl Default for JavaPreset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Preset for JavaPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Java".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/java.svg".to_string()
    }

    fn description(&self) -> String {
        "Java web applications (Spring Boot, Micronaut, Quarkus, etc.)".to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Java provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Java);
        nixpacks.dockerfile(config).await
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        // Delegate to Nixpacks with Java provider
        let nixpacks = NixpacksPreset::new(NixpacksProvider::Java);
        nixpacks.dockerfile_with_build_dir(local_path).await
    }

    fn install_command(&self, local_path: &Path) -> String {
        // Detect build tool and return appropriate command
        if local_path.join("pom.xml").exists() {
            "mvn clean install -DskipTests".to_string()
        } else if local_path.join("build.gradle").exists() || local_path.join("build.gradle.kts").exists() {
            "./gradlew build -x test".to_string()
        } else {
            "mvn clean install -DskipTests".to_string() // Default to Maven
        }
    }

    fn build_command(&self, local_path: &Path) -> String {
        // Detect build tool and return appropriate command
        if local_path.join("pom.xml").exists() {
            "mvn package -DskipTests".to_string()
        } else if local_path.join("build.gradle").exists() || local_path.join("build.gradle.kts").exists() {
            "./gradlew build -x test".to_string()
        } else {
            "mvn package -DskipTests".to_string() // Default to Maven
        }
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Server applications don't need to upload static files
        vec![]
    }

    fn slug(&self) -> String {
        "java".to_string()
    }
}

impl fmt::Display for JavaPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "java")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_java_preset_properties() {
        let preset = JavaPreset::new();
        assert_eq!(preset.label(), "Java");
        assert_eq!(preset.to_string(), "java");
        assert_eq!(preset.slug(), "java");
        assert_eq!(preset.project_type(), ProjectType::Server);
        assert_eq!(preset.icon_url(), "/presets/java.svg");
    }

    #[test]
    fn test_java_maven_commands() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let pom_path = temp_dir.path().join("pom.xml");
        fs::write(&pom_path, "<project></project>").unwrap();

        let preset = JavaPreset::new();
        assert_eq!(preset.install_command(temp_dir.path()), "mvn clean install -DskipTests");
        assert_eq!(preset.build_command(temp_dir.path()), "mvn package -DskipTests");
    }

    #[test]
    fn test_java_gradle_commands() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let gradle_path = temp_dir.path().join("build.gradle");
        fs::write(&gradle_path, "plugins { }").unwrap();

        let preset = JavaPreset::new();
        assert_eq!(preset.install_command(temp_dir.path()), "./gradlew build -x test");
        assert_eq!(preset.build_command(temp_dir.path()), "./gradlew build -x test");
    }

    #[test]
    fn test_java_gradle_kts_commands() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let gradle_path = temp_dir.path().join("build.gradle.kts");
        fs::write(&gradle_path, "plugins { }").unwrap();

        let preset = JavaPreset::new();
        assert_eq!(preset.install_command(temp_dir.path()), "./gradlew build -x test");
        assert_eq!(preset.build_command(temp_dir.path()), "./gradlew build -x test");
    }

    #[test]
    fn test_java_default_commands() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let preset = JavaPreset::new();

        // When no build file exists, default to Maven
        assert_eq!(preset.install_command(temp_dir.path()), "mvn clean install -DskipTests");
        assert_eq!(preset.build_command(temp_dir.path()), "mvn package -DskipTests");
    }
}
