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
    all_presets, detect_node_framework, detect_preset_from_files, get_preset_by_slug,
    DockerfileWithArgs, NixpacksPreset, NixpacksProvider, NodeFramework, PackageManager, Preset,
    PresetConfig, ProjectType,
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
    fn test_no_preset_for_random_files() {
        // Random files should NOT auto-detect any preset
        let files = vec!["some-random-file.txt".to_string(), "src/main.rs".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_none(), "Random files should not auto-detect a preset");
    }

    #[test]
    fn test_detect_nixpacks_with_config() {
        // Nixpacks should only be detected if nixpacks.toml is present
        let files = vec!["nixpacks.toml".to_string(), "main.py".to_string()];
        let preset = detect_preset_from_files(&files);

        assert!(preset.is_some());
        assert_eq!(preset.unwrap().slug(), "nixpacks");
    }

    #[test]
    fn test_detect_presets_from_file_tree_empty() {
        let files: Vec<String> = vec![];
        let presets = detect_presets_from_file_tree(&files);
        assert!(presets.is_empty());
    }

    #[test]
    fn test_detect_presets_from_file_tree_root_only() {
        let files = vec![
            "package.json".to_string(),
            "next.config.js".to_string(),
            "src/app/page.tsx".to_string(),
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].path, "./");
        assert_eq!(presets[0].slug, "nextjs");
        assert_eq!(presets[0].label, "Next.js");
    }

    #[test]
    fn test_detect_presets_from_file_tree_monorepo() {
        let files = vec![
            "package.json".to_string(),
            "apps/web/next.config.js".to_string(),
            "apps/web/package.json".to_string(),
            "apps/api/Dockerfile".to_string(),
            "apps/api/main.go".to_string(),
            "packages/ui/vite.config.ts".to_string(),
            "packages/ui/package.json".to_string(),
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 3);

        // Root should come first
        assert_eq!(presets[0].path, "apps/api");
        assert_eq!(presets[0].slug, "dockerfile");

        assert_eq!(presets[1].path, "apps/web");
        assert_eq!(presets[1].slug, "nextjs");

        assert_eq!(presets[2].path, "packages/ui");
        assert_eq!(presets[2].slug, "vite");
    }

    #[test]
    fn test_detect_presets_from_file_tree_skips_node_modules() {
        let files = vec![
            "next.config.js".to_string(),
            "node_modules/some-package/vite.config.js".to_string(), // Should be ignored
            "src/index.ts".to_string(),
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].slug, "nextjs"); // Only Next.js detected, not Vite
    }

    #[test]
    fn test_detect_presets_from_file_tree_skips_build_dirs() {
        let files = vec![
            "vite.config.ts".to_string(),
            "dist/next.config.js".to_string(), // Should be ignored
            "build/rsbuild.config.ts".to_string(), // Should be ignored
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].slug, "vite");
    }

    #[test]
    fn test_detect_presets_from_file_tree_depth_limit() {
        let files = vec![
            "next.config.js".to_string(),
            "a/b/c/d/e/vite.config.ts".to_string(), // Too deep (5 levels), should be ignored
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].slug, "nextjs");
    }

    #[test]
    fn test_detect_presets_from_file_tree_dockerfile_priority() {
        let files = vec![
            "Dockerfile".to_string(),
            "next.config.js".to_string(),
        ];
        let presets = detect_presets_from_file_tree(&files);

        // Dockerfile has higher priority
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].slug, "dockerfile");
    }

    #[test]
    fn test_detect_presets_from_file_tree_nixpacks_with_config() {
        let files = vec![
            "nixpacks.toml".to_string(),
            "main.py".to_string(),
        ];
        let presets = detect_presets_from_file_tree(&files);

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].slug, "nixpacks");
    }

    #[test]
    fn test_detect_presets_from_file_tree_no_preset_for_random_files() {
        let files = vec![
            "README.md".to_string(),
            "src/utils.js".to_string(), // Generic JS file (no framework config)
            "data.json".to_string(),    // Random JSON file
        ];
        let presets = detect_presets_from_file_tree(&files);

        // No framework-specific config files, so no presets should be detected
        assert!(presets.is_empty());
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

    #[tokio::test]
    async fn test_dockerfile_generation() {
        let temp_dir = create_test_dir_with_files(&["package.json"]);
        let path = temp_dir.path();

        if let Some(preset) = get_preset_by_slug("nextjs") {
            let result = preset
                .dockerfile(DockerfileConfig {
                    use_buildkit: true,
                    root_local_path: path,
                    local_path: path,
                    install_command: Some("npm install"),
                    build_command: Some("npm run build"),
                    output_dir: Some("dist"),
                    build_vars: None,
                    project_slug: "test-project",
                })
                .await;

            // Basic checks that dockerfile contains expected content
            assert!(result.content.contains("FROM"));
            assert!(result.content.contains("npm install"));
            assert!(result.content.contains("npm run build"));
        } else {
            panic!("NextJS preset should be available");
        }
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
