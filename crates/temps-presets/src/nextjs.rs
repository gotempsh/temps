use super::build_system::{BuildSystem, MonorepoTool};
use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use tracing::debug;
use std::path::Path;

pub struct NextJs;

#[async_trait]
impl Preset for NextJs {
    fn slug(&self) -> String {
        "nextjs".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Next.js".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/nextjs.svg".to_string()
    }

    async fn dockerfile(&self, config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        let project_slug = config.project_slug.replace("-", "_").to_lowercase();
        debug!("Local path is {:?}", config.local_path.display());
        let build_system = BuildSystem::detect(config.root_local_path);
        let package_manager = build_system.package_manager;

        // Calculate relative path from root to project directory for monorepos
        let relative_path = if config.local_path != config.root_local_path {
            config.local_path
                .strip_prefix(config.root_local_path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        debug!("Relative path is {:?}", relative_path);

        // Use provided commands or fall back to build system commands
        let build_system_install_cmd = &build_system.get_install_command();
        let mut install_cmd = config.install_command.unwrap_or(build_system_install_cmd).to_string();
        let build_system_build_cmd = &build_system.get_build_command(Some(&project_slug));
        let mut build_cmd = config.build_command.unwrap_or(build_system_build_cmd).to_string();

        // For Bun, ensure we use the full path in cache mount contexts
        if matches!(package_manager, PackageManager::Bun) {
            install_cmd = install_cmd.replace("bun ", "/root/.bun/bin/bun ");
            build_cmd = build_cmd.replace("bun ", "/root/.bun/bin/bun ");
        }

        let (base_image, start_cmd, run_image) = match package_manager {
            PackageManager::Bun => ("node:22", "npx next start", "node:22-alpine"),
            PackageManager::Yarn => ("node:22-alpine", "npx next start", "node:22-alpine"),
            _ => ("node:22", "npx next start", "node:22-alpine"),
        };

        // Determine cache path based on whether it's a monorepo subproject
        let cache_path = if !relative_path.is_empty() {
            format!("/{project_slug}/{relative_path}/.next/cache")
        } else {
            format!("/{project_slug}/.next/cache")
        };

        // Prepare package manager installation commands if needed
        let bun_setup = if matches!(package_manager, PackageManager::Bun) {
            r#"# Add Bun installation if needed
RUN apt-get update && apt-get install -y curl unzip
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

"#
        } else if matches!(package_manager, PackageManager::Yarn) {
            r#"# Enable corepack for Yarn Berry
RUN corepack enable

"#
        } else if matches!(package_manager, PackageManager::Pnpm) {
            r#"# Enable corepack for pnpm
RUN corepack enable
RUN corepack prepare pnpm@latest --activate

"#
        } else {
            ""
        };

        // Determine the working directory - for monorepos with subdirectories,
        // we copy everything to /{project_slug} but then work in the subdirectory
        let workdir = if !relative_path.is_empty() && !matches!(build_system.monorepo_tool, MonorepoTool::None) {
            format!("/{project_slug}/{relative_path}")
        } else {
            format!("/{project_slug}")
        };

        // Cache setup command depends on BuildKit availability
        let cache_setup_cmd = if config.use_buildkit {
            format!(
                "RUN --mount=type=cache,target={},id=next_cache_{} \\\n    mkdir -p {}",
                cache_path, project_slug, cache_path
            )
        } else {
            format!("RUN mkdir -p {}", cache_path)
        };

        let mut dockerfile = format!(
            r#"# syntax=docker/dockerfile:1.4

# Stage 1: Build
FROM {base_image} AS build
WORKDIR /{project_slug}

{bun_setup}# Setup caching for Next.js
{cache_setup}

"#,
            base_image = base_image,
            project_slug = project_slug,
            bun_setup = bun_setup,
            cache_setup = cache_setup_cmd,
        );

        // For monorepos, we need to copy the entire repository
        match build_system.monorepo_tool {
            MonorepoTool::None => {
                dockerfile.push_str("# Copy and install dependencies\nCOPY package*.json .\n");

                // Add lock files and package manager configurations
                match package_manager {
                    PackageManager::Bun => dockerfile.push_str("COPY bun.lock* .\n"),
                    PackageManager::Yarn => {
                        dockerfile.push_str("COPY yarn.lock .\n");
                        // Copy Yarn Berry configuration files if they exist
                        dockerfile.push_str("COPY .yarnrc.yml* .\n");
                        dockerfile.push_str("COPY .yarn* ./.yarn/\n");
                    },
                    PackageManager::Pnpm => dockerfile.push_str("COPY pnpm-lock.yaml .\n"),
                    _ => {}
                }
            }
            _ => {
                dockerfile.push_str("# Copy entire repository for monorepo build\nCOPY . .\n");

                // Change to subdirectory if this is a monorepo subproject
                if !relative_path.is_empty() {
                    dockerfile.push_str(&format!("\n# Change to project subdirectory\nWORKDIR {}\n", workdir));
                }
            }
        }

        // Install command depends on BuildKit availability
        let install_cmd_line = if config.use_buildkit {
            format!(
                "RUN --mount=type=cache,target=/{}/cache/node_modules,id=node_modules_{} {}",
                project_slug, project_slug, install_cmd
            )
        } else {
            format!("RUN {}", install_cmd)
        };

        dockerfile.push_str(&format!(
            r#"
# Install dependencies
{}
"#,
            install_cmd_line,
        ));

        // For non-monorepos, copy remaining files after install
        if matches!(build_system.monorepo_tool, MonorepoTool::None) {
            dockerfile.push_str("\n# Copy project files\nCOPY . .\n");
        }

        // Add build variables if present
        if let Some(vars) = config.build_vars {
            for var in vars {
                dockerfile.push_str(&format!("ARG {}\n", var));
            }
        }

        // Build command depends on BuildKit availability
        let build_cmd_line = if config.use_buildkit {
            format!(
                "RUN --mount=type=cache,target={},id=next_cache_{} \\\n    {}",
                cache_path, project_slug, build_cmd
            )
        } else {
            format!("RUN {}", build_cmd)
        };

        dockerfile.push_str(&format!(
            r#"
# Build the application
{}

# Stage 2: Production
FROM {run_image}
WORKDIR /{project_slug}

RUN apk update && apk add curl

"#,
            build_cmd_line,
            project_slug = project_slug,
            run_image = run_image,
        ));

        // For monorepos, we need to copy only the specific project's built files
        match build_system.monorepo_tool {
            MonorepoTool::None => {
                dockerfile.push_str(&format!(
                    "# Copy built files from build stage\nCOPY --from=build /{project_slug}/ /{project_slug}/\n",
                    project_slug = project_slug
                ));
                // Copy lock file in production stage if present
                match package_manager {
                    PackageManager::Bun => dockerfile.push_str(&format!(
                        "COPY --from=build /{project_slug}/bun.lock* /{project_slug}/bun.lock*\n",
                        project_slug = project_slug
                    )),
                    PackageManager::Yarn => dockerfile.push_str(&format!(
                        "COPY --from=build /{project_slug}/yarn.lock /{project_slug}/yarn.lock\n",
                        project_slug = project_slug
                    )),
                    _ => {}
                }
            }
            _ => {
                let project_path = format!("/{project_slug}");

                dockerfile.push_str(&format!(
                    r#"# Copy the entire monorepo project
COPY --from=build {project_path} /{project_slug}

# Set working directory to the project path
WORKDIR /{project_slug}/{relative_path}
"#,
                    project_path = project_path,
                    project_slug = project_slug,
                    relative_path = relative_path
                ));
            }
        }



        dockerfile.push_str(
            r#"
# Set production environment
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV HOSTNAME=0.0.0.0
ENV HOST=0.0.0.0

EXPOSE 3000

"#,
        );

        // Add start command
        dockerfile.push_str(&format!("CMD [\"{}\"]", start_cmd.replace(" ", "\", \"")));

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, _local_path: &Path) -> DockerfileWithArgs {
        let content = r#"
# Use a lightweight Node.js image as the base
FROM node:22-alpine AS runner

WORKDIR /app

# Add curl for health checks
RUN apk add --no-cache curl

# Set environment to production
ENV NODE_ENV production

# Copy the built Next.js application
COPY .next/standalone ./
COPY .next/static ./.next/static
COPY public ./public
COPY next.config.* ./

# Expose the port the app runs on
EXPOSE 3000

# Start the Next.js application
CMD ["node", "server.js"]
"#;
        DockerfileWithArgs::new(content.to_string())
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
        vec![
            "package*.json".to_string(),
            "next.config.*".to_string(),
            "public".to_string(),
            ".next".to_string(),
        ]
    }
}

impl std::fmt::Display for NextJs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DockerfileConfig;

    #[tokio::test]
    async fn test_bun_dockerfile_uses_full_path() {
        // Create a temp directory with bun.lock to trigger Bun detection
        let temp_dir = std::env::temp_dir().join("test_nextjs_bun");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("bun.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &temp_dir,
            local_path: &temp_dir,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        }).await;

        // Verify Bun is installed
        assert!(result.content.contains("curl -fsSL https://bun.sh/install | bash"));
        assert!(result.content.contains("ENV PATH=\"/root/.bun/bin:${PATH}\""));

        // Verify commands use full path to bun
        assert!(result.content.contains("/root/.bun/bin/bun install"));
        assert!(result.content.contains("/root/.bun/bin/bun run build"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_npm_dockerfile_no_bun_installation() {
        // Create a temp directory with package-lock.json to trigger npm detection
        let temp_dir = std::env::temp_dir().join("test_nextjs_npm");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("package-lock.json"), "").unwrap();

        let preset = NextJs;
        let result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &temp_dir,
            local_path: &temp_dir,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        }).await;

        // Verify Bun is NOT installed
        assert!(!result.content.contains("curl -fsSL https://bun.sh/install | bash"));

        // Verify npm commands are used
        assert!(result.content.contains("npm install") || result.content.contains("npm ci"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_monorepo_subdirectory_build() {
        // Create a temp monorepo structure
        let temp_dir = std::env::temp_dir().join("test_nextjs_monorepo");
        let subproject_dir = temp_dir.join("apps").join("web");
        std::fs::create_dir_all(&subproject_dir).unwrap();

        // Add turbo.json to trigger monorepo detection
        std::fs::write(temp_dir.join("turbo.json"), "{}").unwrap();
        std::fs::write(subproject_dir.join("package.json"), "{}").unwrap();

        let preset = NextJs;
        let result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &temp_dir,
            local_path: &subproject_dir,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        }).await;

        // Verify the entire repository is copied for monorepos
        assert!(result.content.contains("# Copy entire repository for monorepo build"));
        assert!(result.content.contains("COPY . ."));

        // Verify WORKDIR is set to the subdirectory in build stage
        assert!(result.content.contains("# Change to project subdirectory"));
        assert!(result.content.contains("WORKDIR /test_project/apps/web"));

        // Verify WORKDIR is set to the subdirectory in production stage
        assert!(result.content.contains("# Set working directory to the project path"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_non_monorepo_no_subdirectory_workdir() {
        // Create a simple Next.js project (not a monorepo)
        let temp_dir = std::env::temp_dir().join("test_nextjs_simple");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("package.json"), "{}").unwrap();

        let preset = NextJs;
        let result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &temp_dir,
            local_path: &temp_dir,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        }).await;

        // Verify only one WORKDIR is set (the initial one) - no subdirectory WORKDIR
        let workdir_count = result.content.matches("WORKDIR /test_project").count();
        assert_eq!(workdir_count, 2); // Once in build stage, once in production stage

        // Verify no subdirectory change
        assert!(!result.content.contains("# Change to project subdirectory"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_custom_install_and_build_commands_with_bun() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_custom_bun");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("bun.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &temp_dir,
            local_path: &temp_dir,
            install_command: Some("bun install --frozen-lockfile"),
            build_command: Some("bun run build:prod"),
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        }).await;

        // Verify custom commands are used with full bun path
        assert!(result.content.contains("/root/.bun/bin/bun install --frozen-lockfile"));
        assert!(result.content.contains("/root/.bun/bin/bun run build:prod"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
