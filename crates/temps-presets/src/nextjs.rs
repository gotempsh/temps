use super::build_system::{BuildSystem, MonorepoTool};
use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use tracing::debug;
use std::path::Path;

/// Google's distroless Node.js image - contains ONLY Node.js runtime.
/// No shell, no package manager, no OS utilities = maximum security.
/// Used for standalone Next.js builds via `dockerfile_with_build_dir`.
/// See: https://github.com/GoogleContainerTools/distroless
const DISTROLESS_NODEJS: &str = "gcr.io/distroless/nodejs22-debian12:nonroot";

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

        // Use explicit path to Next.js binary for distroless
        // Distroless has /nodejs/bin/node as entrypoint, so CMD just needs the script path
        // This is equivalent to `npm start` / `next start` but works in distroless (no npm/shell needed)
        let start_cmd = format!("/{}/node_modules/next/dist/bin/next\", \"start", project_slug);

        // Build stage uses full Node.js image with package managers
        let base_image = match package_manager {
            PackageManager::Bun => "node:22",  // Bun needs apt for installation
            PackageManager::Yarn => "node:22-alpine",
            _ => "node:22",
        };

        // Production stage uses distroless for maximum security
        // No shell, no wget, no package managers = minimal attack surface
        let run_image = DISTROLESS_NODEJS;

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
RUN corepack prepare pnpxm@latest --activate

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

# Ensure public directory exists for COPY command
RUN mkdir -p public

# NOTE: We do NOT prune devDependencies for Next.js projects
# Next.js needs TypeScript and other dev tools at runtime when using:
# - next.config.ts (requires typescript)
# - ESLint configs
# - Custom build tools
# Since we're using distroless (no shell/npm), Next.js cannot auto-install missing packages.
# The slight increase in image size is acceptable given we're already using ultra-secure distroless base.

# Stage 2: Production using Google's Distroless image
# No shell, no wget, no package managers = maximum security
FROM {run_image} AS runner
WORKDIR /{project_slug}

# Distroless :nonroot tag runs as uid 65532 (no RUN commands possible - no shell)

"#,
            build_cmd_line,
            project_slug = project_slug,
            run_image = run_image,
        ));

        // Copy entire project from build stage
        // This ensures all runtime files are available (drizzle, config, mydata, locales, etc.)
        // Distroless uses uid 65532 (nonroot user)
        match build_system.monorepo_tool {
            MonorepoTool::None => {
                dockerfile.push_str(&format!(
                    r#"# Copy entire project directory to ensure all runtime files are available
# This includes: node_modules, .next, public, and ANY custom directories (drizzle, mydata, etc.)
COPY --from=build --chown=65532:65532 /{project_slug} /{project_slug}
"#,
                    project_slug = project_slug
                ));
            }
            _ => {
                // For monorepos, copy the subdirectory
                dockerfile.push_str(&format!(
                    r#"# Copy entire project directory to ensure all runtime files are available
# This includes: node_modules, .next, public, and ANY custom directories (drizzle, mydata, etc.)
COPY --from=build --chown=65532:65532 /{project_slug}/{relative_path} /{project_slug}
"#,
                    project_slug = project_slug,
                    relative_path = relative_path
                ));
            }
        }

        // Set environment (distroless :nonroot already runs as uid 65532)
        dockerfile.push_str(
            r#"
# Set production environment
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV HOSTNAME=0.0.0.0
ENV PORT=3000

EXPOSE 3000

"#,
        );

        // Add start command - distroless has node as entrypoint
        dockerfile.push_str(&format!("CMD [\"{}\"]", start_cmd));

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, _local_path: &Path) -> DockerfileWithArgs {
        // Use distroless for maximum security - no shell, no package manager, no attack surface
        let content = format!(r#"
# Use Google's Distroless Node.js image - contains ONLY Node.js runtime
# No shell, no package manager, no OS utilities = maximum security
# See: https://github.com/GoogleContainerTools/distroless
FROM {distroless} AS runner

WORKDIR /app

# Set environment to production
ENV NODE_ENV=production

# Copy the built Next.js standalone application
# Distroless :nonroot runs as uid 65532
COPY --chown=65532:65532 .next/standalone ./
COPY --chown=65532:65532 .next/static ./.next/static
COPY --chown=65532:65532 public ./public

# Expose the port the app runs on
EXPOSE 3000

# Start the Next.js application
# Note: Distroless nodejs images automatically use node as entrypoint
CMD ["server.js"]
"#, distroless = DISTROLESS_NODEJS);
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

        // Verify entire project directory is copied in production stage (not selective files)
        assert!(result.content.contains("# Copy entire project directory to ensure all runtime files are available"));
        assert!(result.content.contains("# This includes: node_modules, .next, public, and ANY custom directories (drizzle, mydata, etc.)"));
        // Verify the copy is from the subdirectory path (apps/web)
        assert!(result.content.contains("COPY --from=build --chown=65532:65532 /test_project/apps/web /test_project"));

        // Verify we do NOT prune devDependencies (TypeScript needed at runtime)
        assert!(result.content.contains("# NOTE: We do NOT prune devDependencies for Next.js projects"));
        assert!(!result.content.contains("npm prune --production"));
        assert!(!result.content.contains("yarn install --production"));

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
    async fn test_npm_project_uses_distroless() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_npm_distroless");
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

        // Verify distroless is used for runner stage
        assert!(result.content.contains("gcr.io/distroless/nodejs22-debian12:nonroot"));
        // Verify CMD uses explicit path to next start (works in distroless without npm/shell)
        assert!(result.content.contains(r#"CMD ["/test_project/node_modules/next/dist/bin/next", "start"]"#));
        // Verify npm is used in build stage
        assert!(result.content.contains("npm install") || result.content.contains("npm ci"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_bun_project_uses_distroless() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_bun_distroless");
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

        // Verify distroless is used for runner stage
        assert!(result.content.contains("gcr.io/distroless/nodejs22-debian12:nonroot"));
        // Verify CMD uses explicit path to next start (works in distroless without npm/shell)
        assert!(result.content.contains(r#"CMD ["/test_project/node_modules/next/dist/bin/next", "start"]"#));
        // Verify bun is installed in build stage
        assert!(result.content.contains("curl -fsSL https://bun.sh/install | bash"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_yarn_project_uses_distroless() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_yarn_distroless");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("yarn.lock"), "").unwrap();

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

        // Verify distroless is used for runner stage
        assert!(result.content.contains("gcr.io/distroless/nodejs22-debian12:nonroot"));
        // Verify CMD uses explicit path to next start (works in distroless without npm/shell)
        assert!(result.content.contains(r#"CMD ["/test_project/node_modules/next/dist/bin/next", "start"]"#));
        // Verify corepack is enabled for yarn in build stage
        assert!(result.content.contains("corepack enable"));

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

    #[tokio::test]
    async fn test_dockerfile_uses_distroless_with_security() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_distroless_security");
        std::fs::create_dir_all(&temp_dir).unwrap();

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

        // Verify distroless is used for runner stage
        assert!(
            result.content.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless Node.js image"
        );

        // Security: Files owned by distroless nonroot user (uid 65532)
        assert!(
            result.content.contains("--chown=65532:65532"),
            "Should copy files with distroless nonroot user ownership"
        );

        // Security: No RUN commands in production stage (distroless has no shell)
        let production_stage = result.content.split("gcr.io/distroless").nth(1).unwrap_or("");
        assert!(
            !production_stage.contains("\nRUN "),
            "Distroless stage should not have RUN commands"
        );

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_dockerfile_with_build_dir_uses_distroless() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_build_dir_distroless");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let preset = NextJs;
        let result = preset.dockerfile_with_build_dir(&temp_dir).await;

        // Security: Uses Google's distroless image
        assert!(
            result.content.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless Node.js image"
        );

        // Security: Files are copied with distroless nonroot user ownership
        assert!(
            result.content.contains("--chown=65532:65532"),
            "Should copy files with distroless nonroot user ownership"
        );

        // Security: No RUN commands (distroless has no shell)
        assert!(
            !result.content.contains("\nRUN "),
            "Distroless Dockerfile should not have RUN commands"
        );

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// Integration test that builds and runs a real Next.js Docker image
    /// This test requires Docker to be running and may take several minutes.
    /// It uses the fixture at tests/fixtures/nextjs-hello-world
    #[tokio::test]
    async fn test_nextjs_docker_build_and_run() {
        use std::process::Command;
        use std::time::Duration;

        // Check if Docker is available
        let docker_check = Command::new("docker")
            .args(["info"])
            .output();

        if docker_check.is_err() || !docker_check.unwrap().status.success() {
            println!("Docker is not available, skipping test");
            return;
        }

        // Get the fixture path
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set");
        let fixture_path = std::path::PathBuf::from(&manifest_dir)
            .join("tests/fixtures/nextjs-hello-world");

        if !fixture_path.exists() {
            panic!("Fixture not found at {:?}", fixture_path);
        }

        // Create a temp directory and copy the fixture
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let temp_dir = std::env::temp_dir().join(format!("nextjs_docker_test_{}", test_id));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Copy fixture files to temp directory
        let copy_result = Command::new("cp")
            .args(["-r", fixture_path.to_str().unwrap(), temp_dir.to_str().unwrap()])
            .output()
            .expect("Failed to copy fixture");

        if !copy_result.status.success() {
            panic!("Failed to copy fixture: {:?}", String::from_utf8_lossy(&copy_result.stderr));
        }

        let project_dir = temp_dir.join("nextjs-hello-world");

        // Generate Dockerfile using the preset
        let preset = NextJs;
        let dockerfile_result = preset.dockerfile(DockerfileConfig {
            use_buildkit: true,
            root_local_path: &project_dir,
            local_path: &project_dir,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "nextjs-test",
        }).await;

        // Write the Dockerfile
        let dockerfile_path = project_dir.join("Dockerfile");
        std::fs::write(&dockerfile_path, &dockerfile_result.content)
            .expect("Failed to write Dockerfile");

        println!("Generated Dockerfile:\n{}", dockerfile_result.content);

        // Build the Docker image
        let image_name = format!("temps-nextjs-test:{}", test_id);
        println!("Building Docker image: {}", image_name);

        let build_result = Command::new("docker")
            .args([
                "build",
                "-t", &image_name,
                "-f", dockerfile_path.to_str().unwrap(),
                project_dir.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to execute docker build");

        println!("Build stdout:\n{}", String::from_utf8_lossy(&build_result.stdout));
        println!("Build stderr:\n{}", String::from_utf8_lossy(&build_result.stderr));

        if !build_result.status.success() {
            // Cleanup temp directory
            std::fs::remove_dir_all(&temp_dir).ok();
            panic!("Docker build failed: {}", String::from_utf8_lossy(&build_result.stderr));
        }

        // Run the container
        let container_name = format!("temps-nextjs-test-{}", test_id);
        let host_port = 3099; // Use a non-standard port to avoid conflicts

        println!("Starting container: {}", container_name);

        let run_result = Command::new("docker")
            .args([
                "run",
                "-d",
                "--name", &container_name,
                "-p", &format!("{}:3000", host_port),
                &image_name,
            ])
            .output()
            .expect("Failed to execute docker run");

        if !run_result.status.success() {
            // Cleanup
            Command::new("docker").args(["rmi", "-f", &image_name]).output().ok();
            std::fs::remove_dir_all(&temp_dir).ok();
            panic!("Docker run failed: {}", String::from_utf8_lossy(&run_result.stderr));
        }

        // Wait for the container to start and become healthy
        println!("Waiting for container to become ready...");
        let mut attempts = 0;
        let max_attempts = 30;
        let mut is_healthy = false;

        while attempts < max_attempts {
            std::thread::sleep(Duration::from_secs(2));
            attempts += 1;

            // Check container logs for "Ready" message
            let logs_result = Command::new("docker")
                .args(["logs", &container_name])
                .output()
                .expect("Failed to get container logs");

            let logs = String::from_utf8_lossy(&logs_result.stdout);
            let logs_stderr = String::from_utf8_lossy(&logs_result.stderr);

            println!("Attempt {}/{} - Logs: {} {}", attempts, max_attempts, logs, logs_stderr);

            // Check if Next.js is ready
            if logs.contains("Ready") || logs_stderr.contains("Ready") ||
               logs.contains("started server") || logs_stderr.contains("started server") {
                is_healthy = true;
                break;
            }

            // Also try HTTP request
            let curl_result = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("http://localhost:{}", host_port)])
                .output();

            if let Ok(output) = curl_result {
                let status = String::from_utf8_lossy(&output.stdout);
                if status == "200" {
                    is_healthy = true;
                    println!("HTTP health check passed with status 200");
                    break;
                }
            }
        }

        // Get final container logs for debugging
        let final_logs = Command::new("docker")
            .args(["logs", &container_name])
            .output()
            .expect("Failed to get final logs");

        println!("Final container stdout:\n{}", String::from_utf8_lossy(&final_logs.stdout));
        println!("Final container stderr:\n{}", String::from_utf8_lossy(&final_logs.stderr));

        // Check container status
        let inspect_result = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Status}}", &container_name])
            .output()
            .expect("Failed to inspect container");

        let container_status = String::from_utf8_lossy(&inspect_result.stdout).trim().to_string();
        println!("Container status: {}", container_status);

        // Cleanup: Stop and remove container, remove image
        println!("Cleaning up...");
        Command::new("docker").args(["stop", &container_name]).output().ok();
        Command::new("docker").args(["rm", "-f", &container_name]).output().ok();
        Command::new("docker").args(["rmi", "-f", &image_name]).output().ok();
        std::fs::remove_dir_all(&temp_dir).ok();

        // Assert the container was healthy
        assert!(
            is_healthy || container_status == "running",
            "Container did not become healthy within {} seconds. Status: {}",
            max_attempts * 2,
            container_status
        );

        println!("Test passed! Next.js container built and ran successfully.");
    }
}
