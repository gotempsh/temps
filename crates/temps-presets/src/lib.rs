//! Temps Presets - Stateless project detection and Dockerfile generation
//!
//! This crate provides utilities for:
//! - Detecting project types from file patterns
//! - Generating appropriate Dockerfiles for different frameworks
//! - Managing build system configurations

pub use mod_rs::*;

mod mod_rs {
    include!("mod.rs");
}

// Re-export main types for easy access
pub use {
    all_presets, create_custom_preset, detect_preset_from_files, get_preset_by_slug,
    register_docker_custom_preset, PackageManager, Preset, ProjectType,
};

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use tempfile::TempDir;

    /// Helper function to create a temporary directory with test files
    fn create_test_dir_with_files(files: &[&str]) -> TempDir {
        let temp_dir = TempDir::new().unwrap();

        for file_path in files {
            let full_path = temp_dir.path().join(file_path);

            // Create parent directories if needed
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }

            // Create the file with some content
            fs::write(&full_path, "test content").unwrap();
        }

        temp_dir
    }

    #[test]
    fn test_detect_nextjs_preset() {
        let files = vec!["next.config.js".to_string(), "package.json".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "nextjs");
    }

    #[test]
    fn test_detect_vite_preset() {
        let files = vec!["vite.config.js".to_string(), "package.json".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "vite");
    }

    #[test]
    fn test_detect_docker_preset() {
        let files = vec!["Dockerfile".to_string(), "package.json".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "dockerfile");
    }

    #[test]
    fn test_detect_docusaurus_preset() {
        let files = vec![
            "docusaurus.config.js".to_string(),
            "package.json".to_string(),
        ];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "docusaurus");
    }

    #[test]
    fn test_detect_react_app_preset() {
        let files = vec!["package.json".to_string(), "src/react-scripts".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "react-app");
    }

    #[test]
    fn test_detect_rsbuild_preset() {
        let files = vec!["rsbuild.config.ts".to_string(), "package.json".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "rsbuild");
    }

    #[test]
    fn test_detect_custom_preset_fallback() {
        let files = vec!["some-random-file.txt".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "custom");
    }

    #[test]
    fn test_package_manager_detection() {
        // Test pnpm detection
        let temp_dir = create_test_dir_with_files(&["pnpm-lock.yaml"]);
        let package_manager = PackageManager::detect(temp_dir.path());
        assert!(matches!(package_manager, PackageManager::Pnpm));

        // Test npm detection
        let temp_dir = create_test_dir_with_files(&["package-lock.json"]);
        let package_manager = PackageManager::detect(temp_dir.path());
        assert!(matches!(package_manager, PackageManager::Npm));

        // Test yarn detection
        let temp_dir = create_test_dir_with_files(&["yarn.lock"]);
        let package_manager = PackageManager::detect(temp_dir.path());
        assert!(matches!(package_manager, PackageManager::Yarn));

        // Test bun detection
        let temp_dir = create_test_dir_with_files(&["bun.lockb"]);
        let package_manager = PackageManager::detect(temp_dir.path());
        assert!(matches!(package_manager, PackageManager::Bun));

        // Test default (npm) when no lock files
        let temp_dir = create_test_dir_with_files(&["package.json"]);
        let package_manager = PackageManager::detect(temp_dir.path());
        assert!(matches!(package_manager, PackageManager::Npm));
    }

    #[test]
    fn test_package_manager_commands() {
        assert_eq!(PackageManager::Npm.install_command(), "npm install");
        assert_eq!(
            PackageManager::Yarn.install_command(),
            "yarn install --frozen-lockfile"
        );
        assert_eq!(
            PackageManager::Pnpm.install_command(),
            "pnpm install --frozen-lockfile"
        );
        assert_eq!(PackageManager::Bun.install_command(), "bun install");

        assert_eq!(PackageManager::Npm.build_command(), "npm run build");
        assert_eq!(PackageManager::Yarn.build_command(), "yarn build");
        assert_eq!(PackageManager::Pnpm.build_command(), "pnpm run build");
        assert_eq!(PackageManager::Bun.build_command(), "bun run build");
    }

    #[test]
    fn test_package_manager_base_images() {
        assert_eq!(PackageManager::Npm.base_image(), "node:22-alpine");
        assert_eq!(PackageManager::Yarn.base_image(), "node:22-alpine");
        assert_eq!(PackageManager::Pnpm.base_image(), "node:22-alpine");
        assert_eq!(PackageManager::Bun.base_image(), "oven/bun:1.2");
    }

    #[test]
    fn test_get_preset_by_slug() {
        assert!(get_preset_by_slug("nextjs").is_some());
        assert!(get_preset_by_slug("vite").is_some());
        assert!(get_preset_by_slug("dockerfile").is_some());
        assert!(get_preset_by_slug("nonexistent").is_none());
    }

    #[test]
    fn test_all_presets_returns_presets() {
        let presets = all_presets();
        assert!(!presets.is_empty());

        // Check that all expected presets are present
        let slugs: Vec<String> = presets.iter().map(|p| p.slug()).collect();
        assert!(slugs.contains(&"nextjs".to_string()));
        assert!(slugs.contains(&"vite".to_string()));
        assert!(slugs.contains(&"dockerfile".to_string()));
        assert!(slugs.contains(&"docusaurus".to_string()));
    }

    #[test]
    fn test_project_types() {
        assert_eq!(ProjectType::Server.to_string(), "server");
        assert_eq!(ProjectType::Static.to_string(), "static");
    }

    #[test]
    fn test_dockerfile_generation() {
        let temp_dir = create_test_dir_with_files(&["package.json"]);
        let path = temp_dir.path();

        if let Some(preset) = get_preset_by_slug("nextjs") {
            let dockerfile = preset.dockerfile(DockerfileConfig {
                root_local_path: path,
                local_path: path,
                install_command: Some("npm install"),
                build_command: Some("npm run build"),
                output_dir: Some("dist"),
                build_vars: None,
                project_slug: "test-project",
            });

            // Basic checks that dockerfile contains expected content
            assert!(dockerfile.contains("FROM"));
            assert!(dockerfile.contains("npm install"));
            assert!(dockerfile.contains("npm run build"));
        } else {
            panic!("NextJS preset should be available");
        }
    }

    #[test]
    fn test_create_custom_preset() {
        let custom_preset = create_custom_preset(CreateCustomPresetConfig {
            label: "My Custom".to_string(),
            icon_url: "https://example.com/icon.png".to_string(),
            project_type: ProjectType::Server,
            dockerfile: "FROM alpine\nRUN echo 'hello'".to_string(),
            slug: "custom-test".to_string(),
            install_command: "make install".to_string(),
            build_command: "make build".to_string(),
            dockerfile_with_build_dir: "FROM alpine\nWORKDIR /app".to_string(),
        });

        assert_eq!(custom_preset.slug(), "custom-test");
        assert_eq!(custom_preset.label(), "My Custom");
        assert!(matches!(custom_preset.project_type(), ProjectType::Server));
    }

    #[test]
    fn test_preset_priority_docker_first() {
        // Docker should be detected first even if other config files exist
        let files = vec![
            "Dockerfile".to_string(),
            "next.config.js".to_string(),
            "vite.config.js".to_string(),
        ];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "dockerfile");
    }

    #[test]
    fn test_preset_priority_docusaurus_before_nextjs() {
        // Docusaurus should be detected before Next.js if both configs exist
        let files = vec![
            "docusaurus.config.js".to_string(),
            "next.config.js".to_string(),
        ];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "docusaurus");
    }
}
