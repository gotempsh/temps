// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use std::path::Path;

pub struct Vite;

#[async_trait]
impl Preset for Vite {
    fn slug(&self) -> String {
        "vite".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "Vite".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/vite.svg".to_string()
    }

    async fn dockerfile(&self, config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        match super::autopack_preset::pnpm_app_directory(&config) {
            Ok(Some(relative)) => return workspace_dockerfile(&config, &relative),
            Ok(None) => {}
            Err(message) => {
                return DockerfileWithArgs::new(format!(
                "# {}\nFROM node:22\nRUN echo 'Invalid pnpm workspace configuration' >&2; exit 1\n",
                message.replace('\n', "\n# ")
            ))
            }
        }
        let package_manager = PackageManager::detect(config.local_path);
        let install_cmd = config
            .install_command
            .unwrap_or(package_manager.install_command());
        let build_cmd = config
            .build_command
            .unwrap_or(package_manager.build_command());
        let output = config.output_dir.unwrap_or("dist");

        // Use multi-stage build without BuildKit-specific --mount syntax
        let mut dockerfile = format!(
            r#"FROM {} as builder
WORKDIR /app

# Copy package files
COPY package.json package-lock.json* yarn.lock* pnpm-lock.yaml* bun.lockb* ./
{}
# Install dependencies
RUN {}{}

# Copy source code
COPY . .
"#,
            package_manager.base_image(),
            package_manager.dependency_config_copy(config.local_path),
            if matches!(package_manager, PackageManager::Pnpm) {
                "corepack enable && "
            } else {
                ""
            },
            install_cmd
        );

        // Add build variables if present
        if let Some(vars) = config.build_vars {
            for var in vars {
                dockerfile.push_str(&format!("ARG {}\n", var));
            }
        }

        dockerfile.push_str(&format!(
            r#"
# Build application
RUN {}

# Production stage with nginx
FROM nginx:alpine
COPY --from=builder /app/{} /usr/share/nginx/html
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
"#,
            build_cmd, output
        ));

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        let pkg_manager = PackageManager::detect(local_path);

        let content = format!(
            r#"
FROM {}

WORKDIR /app

# Copy only the dist directory
COPY dist ./dist

# Install serve
RUN {}

# Expose the port the app runs on
EXPOSE 3000

CMD ["serve", "-s", "dist", "-l", "3000"]
"#,
            pkg_manager.base_image(),
            match pkg_manager {
                PackageManager::Bun => "bun install -g serve",
                PackageManager::Yarn => "yarn global add serve",
                PackageManager::Npm => "npm install -g serve",
                PackageManager::Pnpm => "npm install -g serve",
            }
        );
        DockerfileWithArgs::new(content)
    }

    fn install_command(&self, local_path: &Path) -> String {
        PackageManager::detect(local_path)
            .install_command()
            .to_string()
    }

    fn build_command(&self, local_path: &Path) -> String {
        PackageManager::detect(local_path)
            .build_command()
            .to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec!["/usr/share/nginx/html".to_string()]
    }
    fn default_port(&self) -> u16 {
        5173 // Vite dev server default port
    }

    fn static_output_dir(&self) -> Option<String> {
        Some("/usr/share/nginx/html".to_string())
    }
}

impl std::fmt::Display for Vite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

fn workspace_dockerfile(
    config: &super::DockerfileConfig<'_>,
    relative: &str,
) -> DockerfileWithArgs {
    let install = config
        .install_command
        .unwrap_or("pnpm install --frozen-lockfile");
    let build = config.build_command.map(str::to_owned).unwrap_or_else(|| {
        format!("cd /app && pnpm --filter './{relative}...' --if-present run build")
    });
    let mut dockerfile = format!("FROM node:22 AS builder\nWORKDIR /app\nRUN corepack enable\nCOPY . .\nENV CI=true\nRUN {install}\nWORKDIR /app/{relative}\n");
    for variable in config.build_vars.into_iter().flatten() {
        dockerfile.push_str(&format!("ARG {variable}\n"));
    }
    let output = config.output_dir.unwrap_or("dist");
    dockerfile.push_str(&format!("RUN {build}\nFROM nginx:alpine\nCOPY --from=builder /app/{relative}/{output} /usr/share/nginx/html\nEXPOSE 80\nCMD [\"nginx\", \"-g\", \"daemon off;\"]\n"));
    DockerfileWithArgs::new(dockerfile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DockerfileConfig;

    #[tokio::test]
    async fn nested_pnpm_vite_installs_root_and_builds_dependency_graph() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*, packages/*]",
        )
        .unwrap();
        std::fs::write(repo.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
        let result = Vite
            .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
            .await
            .content;
        assert!(result.contains("corepack enable"));
        assert!(result.contains("COPY . .\nENV CI=true\nRUN pnpm install --frozen-lockfile"));
        assert!(result.contains("pnpm --filter './apps/web...' --if-present run build"));
        assert!(result.contains("/app/apps/web/dist /usr/share/nginx/html"));
    }

    #[tokio::test]
    async fn workspace_overrides_run_install_at_root_and_build_in_app() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*]",
        )
        .unwrap();
        let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
        config.install_command = Some("pnpm install && pnpm run prepare");
        config.build_command = Some("pnpm run release");
        config.output_dir = Some("public");
        let result = Vite.dockerfile(config).await.content;
        assert!(result.contains(
            "RUN pnpm install && pnpm run prepare\nWORKDIR /app/apps/web\nRUN pnpm run release"
        ));
        assert!(result.contains("/app/apps/web/public /usr/share/nginx/html"));
    }

    #[tokio::test]
    async fn extglob_vite_members_receive_root_install_and_dependency_builds() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: ['apps/@(web|api|private)', '!apps/@(private|internal)']",
        )
        .unwrap();
        for (name, member) in [
            ("web", true),
            ("api", true),
            ("private", false),
            ("mobile", false),
        ] {
            let app = repo.path().join("apps").join(name);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("package.json"), "{}").unwrap();
            let result = Vite
                .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
                .await
                .content;
            assert_eq!(
                result.contains("pnpm install --frozen-lockfile"),
                member,
                "{name}: {result}"
            );
            assert_eq!(result.contains("--filter"), member, "{name}: {result}");
        }
    }

    #[tokio::test]
    async fn nonmember_vite_apps_never_receive_workspace_filters() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        for packages in [
            "packages: [packages/*]",
            "packages: ['apps/*', '!apps/web']",
        ] {
            std::fs::write(repo.path().join("pnpm-workspace.yaml"), packages).unwrap();
            let result = Vite
                .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
                .await
                .content;
            assert!(!result.contains("--filter"), "{result}");
            assert!(!result.contains("WORKDIR /app/apps/web"), "{result}");
            assert!(result.contains("RUN npm install"), "{result}");
        }
    }

    #[tokio::test]
    async fn standalone_pnpm_enables_pinned_manager_without_workspace_filter() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
        let result = Vite
            .dockerfile(DockerfileConfig::new(repo.path(), repo.path(), "fixture"))
            .await
            .content;
        assert!(result.contains("RUN corepack enable && pnpm install --frozen-lockfile"));
        assert!(!result.contains("--filter"));
    }
}
