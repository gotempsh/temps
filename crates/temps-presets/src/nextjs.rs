// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::build_system::{BuildSystem, MonorepoTool};
use super::workspace_manifests::{self, InstallManifests};
use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use std::path::Path;
use tracing::debug;

/// Security hardening for Node.js Alpine runner
/// This approach provides security while maintaining compatibility:
/// - Removes package managers to prevent runtime package installation
/// - Creates a dedicated non-root user (nodejs:nodejs with UID/GID 1001)
/// - Keeps CA certificates for HTTPS support (unlike distroless)
/// - Maintains shell access for debugging if needed
const NODEJS_ALPINE_SECURITY_HARDENING: &str = r#"# Security hardening - remove package manager and run as non-root
# Create non-root user for running the application
RUN addgroup --system --gid 1001 nodejs && \
    adduser --system --uid 1001 nodejs && \
    # Remove package managers to prevent runtime package installation
    rm -rf /sbin/apk /usr/bin/apk /etc/apk /var/cache/apk /lib/apk && \
    rm -rf /var/lib/apt /usr/bin/apt* /usr/bin/dpkg* 2>/dev/null || true

USER nodejs"#;

const NODEJS_DEBIAN_SECURITY_HARDENING: &str = r#"RUN groupadd --system --gid 1001 nodejs && \
    useradd --system --uid 1001 --gid 1001 nodejs && \
    rm -f /usr/local/bin/npm /usr/local/bin/npx /usr/local/bin/corepack && \
    rm -rf /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/corepack
USER nodejs"#;

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
        let project_slug: String = config
            .project_slug
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        debug!("Local path is {:?}", config.local_path.display());
        let build_system = BuildSystem::detect(config.root_local_path);
        let package_manager = build_system.package_manager;

        // Calculate relative path from root to project directory for monorepos
        let relative_path = if config.local_path != config.root_local_path {
            config
                .local_path
                .strip_prefix(config.root_local_path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        if !relative_path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
            || Path::new(&relative_path)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return DockerfileWithArgs::new("FROM node:22\nRUN echo 'Unsupported Next.js app directory: use letters, numbers, slash, dot, dash or underscore' >&2; exit 1\n".to_string());
        }

        debug!("Relative path is {:?}", relative_path);

        // Use provided commands or fall back to build system commands
        let build_system_install_cmd = &build_system.get_install_command();
        let mut install_cmd = config
            .install_command
            .unwrap_or(build_system_install_cmd)
            .to_string();
        let default_build_command = if !relative_path.is_empty()
            && matches!(build_system.monorepo_tool, MonorepoTool::Turbo)
        {
            // A direct app build misses generated outputs from its workspace
            // dependencies. Run the selected app's dependency graph at root.
            format!(
                "cd /{project_slug} && {} --filter='{{./{relative_path}}}...'",
                build_system.get_build_command(None)
            )
        } else if !relative_path.is_empty()
            && !matches!(build_system.monorepo_tool, MonorepoTool::None)
        {
            package_manager.build_command().to_string()
        } else {
            build_system.get_build_command(Some(&project_slug))
        };
        let build_system_build_cmd = &default_build_command;
        let mut build_cmd = config
            .build_command
            .unwrap_or(build_system_build_cmd)
            .to_string();

        // For Bun, ensure we use the full path in cache mount contexts
        if matches!(package_manager, PackageManager::Bun) {
            install_cmd = install_cmd.replace("bun ", "/root/.bun/bin/bun ");
            build_cmd = build_cmd.replace("bun ", "/root/.bun/bin/bun ");
        }

        // Build stage uses full Node.js image with package managers
        let base_image = match package_manager {
            PackageManager::Bun => "node:22",
            PackageManager::Yarn => "node:22-alpine",
            _ => "node:22",
        };

        // Match the build stage libc so native modules load in production.
        // Secure: non-root user, package manager removed, proper HTTPS support
        let (run_image, runtime_hardening) = if base_image.ends_with("-alpine") {
            ("node:22-alpine", NODEJS_ALPINE_SECURITY_HARDENING)
        } else {
            ("node:22-slim", NODEJS_DEBIAN_SECURITY_HARDENING)
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
COPY --from=bun-tooling /usr/local/bin/bun /root/.bun/bin/bun
ENV PATH="/root/.bun/bin:${PATH}"

"#
        } else if matches!(package_manager, PackageManager::Yarn) {
            r#"# Enable corepack for Yarn Berry
RUN corepack enable

"#
        } else if matches!(package_manager, PackageManager::Pnpm) {
            r#"# Enable corepack for pnpm
RUN corepack enable

"#
        } else {
            ""
        };

        // Determine the working directory - for monorepos with subdirectories,
        // we copy everything to /{project_slug} but then work in the subdirectory
        let workdir = if !relative_path.is_empty()
            && !matches!(build_system.monorepo_tool, MonorepoTool::None)
        {
            format!("/{project_slug}/{relative_path}")
        } else {
            format!("/{project_slug}")
        };

        // Cache setup command depends on BuildKit availability. Every cache
        // mount is `sharing=locked`: two builds of the same project and ref
        // writing `.next/cache` or the package store at once can corrupt it.
        let cache_setup_cmd = if config.use_buildkit {
            format!(
                "RUN --mount=type=cache,target={},id=next_cache_{},sharing=locked \\\n    mkdir -p {}",
                cache_path, project_slug, cache_path
            )
        } else {
            format!("RUN mkdir -p {}", cache_path)
        };

        let bun_stage = if matches!(package_manager, PackageManager::Bun) {
            "FROM oven/bun:1 AS bun-tooling\n\n"
        } else {
            ""
        };

        let mut dockerfile = format!(
            r#"# syntax=docker/dockerfile:1.4

{bun_stage}# Stage 1: Build
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

        // Whether the whole repository is already in the image when the
        // install runs. When it is not, it is copied right after the install.
        let mut source_copied_before_install = false;

        match build_system.monorepo_tool {
            MonorepoTool::None | MonorepoTool::Turbo | MonorepoTool::Workspace
                if config.install_command.is_some() =>
            {
                // Custom installs can invoke scripts or read arbitrary source files.
                dockerfile
                    .push_str("# Copy entire repository for custom install command\nCOPY . .\n");
                source_copied_before_install = true;
            }
            MonorepoTool::None | MonorepoTool::Turbo | MonorepoTool::Workspace => {
                match workspace_manifests::collect(config.root_local_path) {
                    Ok(manifests) => {
                        dockerfile.push_str(&workspace_manifest_copies(&manifests));
                    }
                    Err(fallback) => {
                        debug!(
                            "Copying the whole repository before install: {}",
                            fallback.reason
                        );
                        // The reason quotes repository paths. A newline in one
                        // would end the comment and start a Dockerfile instruction.
                        let reason = fallback.reason.replace(|c: char| c.is_control(), " ");
                        dockerfile.push_str(&format!(
                            "# Copy entire repository for monorepo build\n\
                         # (before install: {reason})\nCOPY . .\n"
                        ));
                        source_copied_before_install = true;
                    }
                }
            }
            // Lerna and Nx run their own tooling to install, which reads
            // project configuration from anywhere in the repository.
            MonorepoTool::Lerna | MonorepoTool::Nx => {
                dockerfile.push_str("# Copy entire repository for monorepo build\nCOPY . .\n");
                source_copied_before_install = true;
            }
        }

        // Overrides are authored relative to the selected app. Default
        // workspace installs above run from the root to use its lockfile.
        if config.install_command.is_some()
            && !relative_path.is_empty()
            && !matches!(build_system.monorepo_tool, MonorepoTool::None)
        {
            dockerfile.push_str(&format!("WORKDIR {workdir}\n"));
        }

        // With BuildKit, the package manager's download store lives in a
        // cache mount, so when the lockfile changes only new or changed
        // packages are downloaded. `node_modules` itself stays in the layer:
        // the runtime stage copies it.
        let install_cmd_line = if config.use_buildkit {
            let (store_dir, store_env) = package_manager.store_cache();
            let env_lines: String = store_env
                .iter()
                .map(|(key, value)| format!("ENV {key}={value}\n"))
                .collect();
            format!(
                "# Package download store, kept between builds\n{env_lines}\
                 RUN --mount=type=cache,target={store_dir},id={pm}_store_{project_slug},sharing=locked {install_cmd}",
                pm = package_manager.id(),
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

        // Copy the sources after the install, so that changing them does not
        // invalidate the install layer.
        if !source_copied_before_install {
            if relative_path.is_empty() || matches!(build_system.monorepo_tool, MonorepoTool::None)
            {
                dockerfile.push_str("\n# Copy project files\nCOPY . .\n");
            } else {
                // WORKDIR may be a subproject by now; copy from the root.
                dockerfile.push_str(&format!(
                    "\n# Copy the rest of the repository\nCOPY . /{project_slug}/\n"
                ));
            }
        }

        if !relative_path.is_empty() && !matches!(build_system.monorepo_tool, MonorepoTool::None) {
            dockerfile.push_str(&format!(
                "\n# Change to project subdirectory\nWORKDIR {workdir}\n"
            ));
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
                "RUN --mount=type=cache,target={},id=next_cache_{},sharing=locked \\\n    {}",
                cache_path, project_slug, build_cmd
            )
        } else {
            format!("RUN {}", build_cmd)
        };

        // Normalize the runtime tree after the build. Standalone mode is
        // selected from actual output, so functional/TypeScript configs work
        // without parsing or changing the user's Next.js configuration.
        // `.git` stays in the build stage for builds that read VCS metadata,
        // but never reaches the runtime image: the `next start` fallback
        // copies the whole tree, and repository history must not ship.
        let app_relative = if matches!(build_system.monorepo_tool, MonorepoTool::None) {
            ""
        } else {
            relative_path.as_str()
        };
        let runtime_app = if app_relative.is_empty() {
            "/temps-runtime".to_string()
        } else {
            format!("/temps-runtime/{app_relative}")
        };
        dockerfile.push_str(&format!(
            r#"
# Build the application
{build_cmd_line}

# Package standalone output when available; retain all files for next start.
RUN mkdir -p public {runtime_app} && \
    if [ -f .next/standalone/{app_relative}/server.js ]; then \
        cp -a .next/standalone/. /temps-runtime/ && \
        mkdir -p {runtime_app}/public && cp -a public/. {runtime_app}/public/ && \
        mkdir -p {runtime_app}/.next && \
        cp -a .next/static {runtime_app}/.next/static && \
        printf '%s\n' '#!/bin/sh' 'exec node server.js' > /temps-start.sh; \
    elif [ -f .next/standalone/server.js ]; then \
        cp -a .next/standalone/. {runtime_app}/ && \
        mkdir -p {runtime_app}/public && cp -a public/. {runtime_app}/public/ && \
        mkdir -p {runtime_app}/.next && \
        cp -a .next/static {runtime_app}/.next/static && \
        printf '%s\n' '#!/bin/sh' 'exec node server.js' > /temps-start.sh; \
    else \
        cp -a /{project_slug}/. /temps-runtime/ && \
        printf '%s\n' '#!/bin/sh' "exec node \"\$(node -p \"require.resolve('next/dist/bin/next')\")\" start" > /temps-start.sh; \
    fi && \
    find /temps-runtime -name .npmrc -type f -delete && \
    find /temps-runtime -name .git -prune -exec rm -rf {{}} +

# Stage 2: Production
FROM {run_image} AS runner
WORKDIR {workdir}

{alpine_hardening}

COPY --from=build --chown=nodejs:nodejs /temps-runtime/ /{project_slug}/
COPY --from=build /temps-start.sh /temps-start.sh

ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV HOSTNAME=0.0.0.0
ENV PORT=3000
EXPOSE 3000
CMD ["sh", "/temps-start.sh"]
"#,
            alpine_hardening = runtime_hardening,
        ));

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, _local_path: &Path) -> DockerfileWithArgs {
        // Use hardened Alpine for security with full CA certificate support
        let content = format!(
            r#"
# Use hardened Alpine Node.js image
# Secure: non-root user, package manager removed, full CA certificates for HTTPS
FROM node:22-alpine AS runner

WORKDIR /app

# Set environment to production
ENV NODE_ENV=production

{alpine_hardening}

# Copy the built Next.js standalone application
# Alpine uses nodejs:nodejs user (uid 1001)
COPY --chown=nodejs:nodejs .next/standalone ./
COPY --chown=nodejs:nodejs .next/static ./.next/static
COPY --chown=nodejs:nodejs public ./public

# Expose the port the app runs on
EXPOSE 3000

# Start the Next.js application
CMD ["node", "server.js"]
"#,
            alpine_hardening = NODEJS_ALPINE_SECURITY_HARDENING
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
        vec![
            "package*.json".to_string(),
            "next.config.*".to_string(),
            "public".to_string(),
            ".next".to_string(),
        ]
    }
}

/// `COPY` lines for a workspace's install inputs, in JSON form so paths with
/// spaces survive.
///
/// Optional root files are copied with a trailing `*`: BuildKit accepts a
/// wildcard that matches nothing, so a lockfile or `.npmrc` excluded by
/// `.dockerignore` is skipped instead of failing the build.
fn workspace_manifest_copies(manifests: &InstallManifests) -> String {
    let json = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));

    let mut root_sources: Vec<String> = Vec::with_capacity(manifests.root_files.len());
    for file in &manifests.root_files {
        if file == "package.json" {
            root_sources.push(json(file));
        } else {
            root_sources.push(json(&format!("{file}*")));
        }
    }

    if !manifests.root_files.iter().any(|file| file == ".npmrc") {
        root_sources.push(json(".npmrc*"));
    }

    let mut out = String::from(
        "# Copy only what the dependency install reads, so the install below\n\
         # stays cached until a manifest or lockfile changes\n",
    );
    out.push_str(&format!("COPY [{}, \"./\"]\n", root_sources.join(", ")));
    for manifest in &manifests.package_manifests {
        let dir = manifest.trim_end_matches("package.json");
        out.push_str(&format!("COPY [{}, {}]\n", json(manifest), json(dir)));
    }
    for dir in &manifests.dirs {
        out.push_str(&format!(
            "COPY [{}, {}]\n",
            json(dir),
            json(&format!("{dir}/"))
        ));
    }
    out
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
    async fn runtime_packaging_preserves_assets_and_workspace_dependencies() {
        for (app_relative, standalone) in [
            ("", false),
            ("", true),
            ("apps/web", false),
            ("apps/web", true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("project");
            let app = root.join(app_relative);
            std::fs::create_dir_all(app.join(".next/static")).unwrap();
            std::fs::create_dir_all(app.join("public")).unwrap();
            std::fs::create_dir_all(root.join("node_modules/shared")).unwrap();
            std::fs::write(root.join("package.json"), "{}").unwrap();
            std::fs::create_dir_all(root.join(".git/refs")).unwrap();
            std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(root.join(".npmrc"), "//registry.example.test/:_authToken=test-only").unwrap();
            std::fs::write(root.join("node_modules/shared/index.js"), "shared").unwrap();
            std::fs::write(app.join(".next/static/chunk.js"), "chunk").unwrap();
            std::fs::write(app.join("public/asset.txt"), "asset").unwrap();
            if !app_relative.is_empty() {
                std::fs::write(root.join("turbo.json"), "{}").unwrap();
                std::fs::write(app.join("package.json"), "{}").unwrap();
            }
            if standalone {
                let server = app.join(".next/standalone").join(app_relative);
                std::fs::create_dir_all(&server).unwrap();
                std::fs::write(server.join("server.js"), "server").unwrap();
            }
            let output = NextJs
                .dockerfile(DockerfileConfig {
                    root_local_path: &root,
                    local_path: &app,
                    project_slug: "test-project",
                    use_buildkit: false,
                    install_command: None,
                    build_command: None,
                    output_dir: None,
                    build_vars: None,
                })
                .await;
            let script = output
                .content
                .split("# Package standalone output")
                .nth(1)
                .unwrap()
                .split("# Stage 2:")
                .next()
                .unwrap();
            let script = script[script.find("RUN ").unwrap() + 4..]
                .trim()
                .replace(
                    "/temps-runtime",
                    &dir.path().join("runtime").to_string_lossy(),
                )
                .replace(
                    "/temps-start.sh",
                    &dir.path().join("start.sh").to_string_lossy(),
                )
                .replace("/test_project", &root.to_string_lossy());
            let status = std::process::Command::new("sh")
                .args(["-ec", &script])
                .current_dir(&app)
                .status()
                .unwrap();
            assert!(status.success());
            let runtime = dir.path().join("runtime");
            let packaged_app = runtime.join(app_relative);
            assert!(!runtime.join(".npmrc").exists());
            assert!(!runtime.join(".git").exists());
            assert_eq!(
                std::fs::read_to_string(packaged_app.join("public/asset.txt")).unwrap(),
                "asset"
            );
            assert_eq!(
                std::fs::read_to_string(packaged_app.join(".next/static/chunk.js")).unwrap(),
                "chunk"
            );
            let start = std::fs::read_to_string(dir.path().join("start.sh")).unwrap();
            if standalone {
                assert!(start.contains("exec node server.js"));
                assert!(!runtime.join("node_modules/shared").exists());
            } else {
                assert!(start.contains("require.resolve('next/dist/bin/next')"));
                let next = runtime.join("node_modules/next/dist/bin");
                std::fs::create_dir_all(&next).unwrap();
                std::fs::write(next.join("next.js"), "if (process.argv[2] !== 'start') process.exit(1); console.log('started');").unwrap();
                let output = std::process::Command::new("sh").arg(dir.path().join("start.sh")).current_dir(&packaged_app).output().unwrap();
                assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
                assert!(runtime.join("node_modules/shared/index.js").is_file());
            }
        }
    }

    #[tokio::test]
    async fn test_bun_dockerfile_uses_full_path() {
        // Create a temp directory with bun.lock to trigger Bun detection
        let temp_dir = std::env::temp_dir().join("test_nextjs_bun");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("bun.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify Bun is installed
        assert!(result
            .content
            .contains("COPY --from=bun-tooling /usr/local/bin/bun /root/.bun/bin/bun"));
        assert!(result
            .content
            .contains("ENV PATH=\"/root/.bun/bin:${PATH}\""));

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
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify Bun is NOT installed
        assert!(!result
            .content
            .contains("COPY --from=bun-tooling /usr/local/bin/bun /root/.bun/bin/bun"));

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
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &subproject_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify the entire repository is copied for monorepos
        assert!(result
            .content
            .contains("# Copy entire repository for monorepo build"));
        assert!(result.content.contains("COPY . ."));

        // Verify WORKDIR is set to the subdirectory in build stage
        assert!(result.content.contains("# Change to project subdirectory"));
        assert!(result.content.contains("WORKDIR /test_project/apps/web"));

        // Fallback retains the workspace root so hoisted dependencies and
        // pnpm links stay valid, while the server starts from the selected app.
        assert!(result
            .content
            .contains("cp -a /test_project/. /temps-runtime/"));
        assert!(result
            .content
            .contains("COPY --from=build --chown=nodejs:nodejs /temps-runtime/ /test_project/"));
        assert!(result
            .content
            .contains(".next/standalone/apps/web/server.js"));
        assert!(result
            .content
            .contains("npx turbo run build --filter='{./apps/web}...'"));

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
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify only one WORKDIR is set (the initial one) - no subdirectory WORKDIR
        let workdir_count = result.content.matches("WORKDIR /test_project").count();
        assert_eq!(workdir_count, 2); // Once in build stage, once in production stage

        // Verify no subdirectory change
        assert!(!result.content.contains("# Change to project subdirectory"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_npm_project_uses_matching_libc() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_npm_alpine");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("package-lock.json"), "").unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Keep the glibc used by the build stage for native modules
        assert!(result.content.contains("FROM node:22-slim AS runner"));
        // Verify CMD uses node with explicit path to next start
        assert!(result.content.contains(r#"CMD ["sh", "/temps-start.sh"]"#));
        // Verify npm is used in build stage
        assert!(result.content.contains("npm install") || result.content.contains("npm ci"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_bun_project_uses_matching_libc() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_bun_alpine");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("bun.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Keep the glibc used by the build stage for native modules
        assert!(result.content.contains("FROM node:22-slim AS runner"));
        // Verify CMD uses node with explicit path to next start
        assert!(result.content.contains(r#"CMD ["sh", "/temps-start.sh"]"#));
        // Verify bun is installed in build stage
        assert!(result
            .content
            .contains("COPY --from=bun-tooling /usr/local/bin/bun /root/.bun/bin/bun"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_yarn_project_uses_alpine() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_yarn_alpine");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("yarn.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify Alpine is used for runner stage
        assert!(result.content.contains("FROM node:22-alpine AS runner"));
        // Verify CMD uses node with explicit path to next start
        assert!(result.content.contains(r#"CMD ["sh", "/temps-start.sh"]"#));
        // Verify corepack is enabled for yarn in build stage
        assert!(result.content.contains("corepack enable"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_pnpm_project_respects_package_manager_version() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_pnpm_version");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
        std::fs::write(
            temp_dir.join("package.json"),
            r#"{"packageManager":"pnpm@9.15.0"}"#,
        )
        .unwrap();

        let result = NextJs
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        assert!(result.content.contains("corepack enable"));
        assert!(result.content.contains("pnpm install --frozen-lockfile"));
        assert!(!result.content.contains("pnpm@latest"));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_custom_install_and_build_commands_with_bun() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_custom_bun");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("bun.lock"), "").unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: Some("bun install --frozen-lockfile"),
                build_command: Some("bun run build:prod"),
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify custom commands are used with full bun path
        assert!(result
            .content
            .contains("/root/.bun/bin/bun install --frozen-lockfile"));
        assert!(result.content.contains("/root/.bun/bin/bun run build:prod"));

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_dockerfile_uses_debian_with_security() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_alpine_security");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let preset = NextJs;
        let result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &temp_dir,
                local_path: &temp_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await;

        // Verify Alpine is used for runner stage
        assert!(
            result.content.contains("FROM node:22-slim AS runner"),
            "Should use matching Debian Node.js image for runner"
        );

        // Security: Creates non-root user nodejs with UID 1001
        assert!(
            result
                .content
                .contains("useradd --system --uid 1001 --gid 1001 nodejs"),
            "Should create nodejs user with UID 1001"
        );

        // Security: Runs as non-root user
        assert!(
            result.content.contains("USER nodejs"),
            "Should run as nodejs user"
        );

        // Security: Package manager removal
        assert!(
            result.content.contains("rm -f /usr/local/bin/npm"),
            "Should remove npm package manager"
        );

        // Security: Files owned by nodejs user
        assert!(
            result.content.contains("--chown=nodejs:nodejs"),
            "Should copy files with nodejs user ownership"
        );

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_dockerfile_with_build_dir_uses_alpine_with_security() {
        let temp_dir = std::env::temp_dir().join("test_nextjs_build_dir_alpine");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let preset = NextJs;
        let result = preset.dockerfile_with_build_dir(&temp_dir).await;

        // Verify Alpine is used for runner stage
        assert!(
            result.content.contains("FROM node:22-alpine AS runner"),
            "Should use Alpine Node.js image for runner"
        );

        // Security: Creates non-root user nodejs with UID 1001
        assert!(
            result
                .content
                .contains("adduser --system --uid 1001 nodejs"),
            "Should create nodejs user with UID 1001"
        );

        // Security: Runs as non-root user
        assert!(
            result.content.contains("USER nodejs"),
            "Should run as nodejs user"
        );

        // Security: Package manager removal
        assert!(
            result.content.contains("rm -rf /sbin/apk"),
            "Should remove apk package manager"
        );

        // Security: Files owned by nodejs user
        assert!(
            result.content.contains("--chown=nodejs:nodejs"),
            "Should copy files with nodejs user ownership"
        );

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    async fn generate(root: &Path, use_buildkit: bool) -> String {
        NextJs
            .dockerfile(DockerfileConfig {
                use_buildkit,
                root_local_path: root,
                local_path: root,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "test-project",
            })
            .await
            .content
    }

    /// A Turborepo laid out the way the deploy job sees it: the project
    /// directory is the repository root, so root and local path coincide.
    fn turbo_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"name":"repo","scripts":{"build":"turbo run build"}}"#,
        );
        write(root, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        write(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - apps/*\n  - packages/*\n",
        );
        write(root, "turbo.json", "{}");
        write(root, "next.config.js", "module.exports = {}");
        write(root, "apps/web/package.json", r#"{"name":"web"}"#);
        write(root, "packages/ui/package.json", r#"{"name":"@repo/ui"}"#);
        dir
    }

    #[tokio::test]
    async fn install_mounts_each_package_managers_real_download_store() {
        let cases = [
            (
                "package-lock.json",
                "ENV npm_config_cache=/cache/npm\n",
                "RUN --mount=type=cache,target=/cache/npm,id=npm_store_test_project,sharing=locked npm install",
            ),
            (
                "pnpm-lock.yaml",
                "ENV npm_config_store_dir=/cache/pnpm\nENV pnpm_config_store_dir=/cache/pnpm\n",
                "RUN --mount=type=cache,target=/cache/pnpm,id=pnpm_store_test_project,sharing=locked pnpm install --frozen-lockfile",
            ),
            (
                "yarn.lock",
                "ENV YARN_CACHE_FOLDER=/cache/yarn/v1\nENV YARN_GLOBAL_FOLDER=/cache/yarn/berry\n",
                "RUN --mount=type=cache,target=/cache/yarn,id=yarn_store_test_project,sharing=locked yarn install --frozen-lockfile",
            ),
            (
                "bun.lock",
                "ENV BUN_INSTALL_CACHE_DIR=/cache/bun\n",
                "RUN --mount=type=cache,target=/cache/bun,id=bun_store_test_project,sharing=locked /root/.bun/bin/bun install",
            ),
        ];
        for (lockfile, env, install) in cases {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "package.json", "{}");
            write(dir.path(), lockfile, "");
            let dockerfile = generate(dir.path(), true).await;

            assert!(
                dockerfile.contains(env),
                "{lockfile}: missing store env:\n{dockerfile}"
            );
            assert!(
                dockerfile.contains(install),
                "{lockfile}: missing store mount:\n{dockerfile}"
            );
            // The old mount pointed at a directory no package manager uses.
            assert!(!dockerfile.contains("cache/node_modules"), "{dockerfile}");
        }
    }

    #[tokio::test]
    async fn next_cache_mounts_are_locked_against_concurrent_builds() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", "{}");
        let dockerfile = generate(dir.path(), true).await;
        let next_cache_mounts: Vec<&str> = dockerfile
            .lines()
            .filter(|line| line.contains("id=next_cache_test_project"))
            .collect();
        assert_eq!(next_cache_mounts.len(), 2, "{dockerfile}");
        assert!(next_cache_mounts
            .iter()
            .all(|line| line.contains(",sharing=locked")));
    }

    #[tokio::test]
    async fn without_buildkit_there_are_no_cache_mounts_or_store_variables() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", "{}");
        write(dir.path(), "package-lock.json", "");
        let dockerfile = generate(dir.path(), false).await;
        assert!(!dockerfile.contains("--mount"), "{dockerfile}");
        assert!(!dockerfile.contains("npm_config_cache"), "{dockerfile}");
        assert!(dockerfile.contains("RUN npm install"), "{dockerfile}");
    }

    #[tokio::test]
    async fn turbo_workspace_installs_from_manifests_before_copying_sources() {
        let repo = turbo_workspace();
        let dockerfile = generate(repo.path(), true).await;

        let manifests = dockerfile
            .find(r#"COPY ["package.json", "pnpm-lock.yaml*", "pnpm-workspace.yaml*", ".npmrc*", "./"]"#)
            .unwrap_or_else(|| panic!("root install files not copied:\n{dockerfile}"));
        let web = dockerfile
            .find(r#"COPY ["apps/web/package.json", "apps/web/"]"#)
            .unwrap_or_else(|| panic!("workspace manifest not copied:\n{dockerfile}"));
        assert!(dockerfile.contains(r#"COPY ["packages/ui/package.json", "packages/ui/"]"#));
        let install = dockerfile.find("pnpm install").unwrap();
        let sources = dockerfile
            .find("COPY . .")
            .unwrap_or_else(|| panic!("sources not copied after install:\n{dockerfile}"));
        let build = dockerfile.find("pnpm turbo").unwrap();

        assert!(manifests < web && web < install, "{dockerfile}");
        assert!(install < sources && sources < build, "{dockerfile}");
        assert_eq!(dockerfile.matches("COPY . .\n").count(), 1, "{dockerfile}");
    }

    #[tokio::test]
    async fn turbo_subfolder_build_keeps_dependency_tasks() {
        let repo = turbo_workspace();
        let app = repo.path().join("apps/web");
        let output = NextJs
            .dockerfile(DockerfileConfig {
                root_local_path: repo.path(),
                local_path: &app,
                project_slug: "test-project",
                use_buildkit: true,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
            })
            .await;
        assert!(output
            .content
            .contains("cd /test_project && pnpm turbo run build --filter='{./apps/web}...'"));
        assert!(output
            .content
            .contains("target=/test_project/apps/web/.next/cache"));
    }

    #[tokio::test]
    async fn plain_workspaces_install_at_root_and_build_the_selected_app() {
        for pnpm in [false, true] {
            let repo = turbo_workspace();
            std::fs::remove_file(repo.path().join("turbo.json")).unwrap();
            if !pnpm {
                std::fs::remove_file(repo.path().join("pnpm-workspace.yaml")).unwrap();
                std::fs::remove_file(repo.path().join("pnpm-lock.yaml")).unwrap();
                write(
                    repo.path(),
                    "package.json",
                    r#"{"workspaces":["apps/*","packages/*"]}"#,
                );
            }
            let app = repo.path().join("apps/web");
            let output = NextJs
                .dockerfile(DockerfileConfig {
                    root_local_path: repo.path(),
                    local_path: &app,
                    project_slug: "test-project",
                    use_buildkit: false,
                    install_command: None,
                    build_command: None,
                    output_dir: None,
                    build_vars: None,
                })
                .await;
            let manager = if pnpm { "pnpm" } else { "npm" };
            assert!(
                output
                    .content
                    .find(&format!("RUN {manager} install"))
                    .unwrap()
                    < output
                        .content
                        .find("WORKDIR /test_project/apps/web")
                        .unwrap()
            );
            assert!(output.content.contains(&format!("RUN {manager} run build")));
            assert!(output.content.contains("COPY . /test_project/"));
        }
    }

    #[tokio::test]
    async fn yarn_classic_does_not_require_berry_configuration() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "package.json", "{}");
        write(repo.path(), "yarn.lock", "");
        let output = generate(repo.path(), false).await;
        assert!(output.contains(r#"COPY ["package.json", "yarn.lock*", ".npmrc*", "./"]"#));
        assert!(!output.contains("COPY .yarnrc.yml"));
        assert!(!output.contains("COPY .yarn*"));
    }

    #[tokio::test]
    async fn custom_subfolder_install_runs_in_the_selected_app() {
        let repo = turbo_workspace();
        let app = repo.path().join("apps/web");
        let output = NextJs
            .dockerfile(DockerfileConfig {
                root_local_path: repo.path(),
                local_path: &app,
                project_slug: "test-project",
                use_buildkit: false,
                install_command: Some("node scripts/install.js"),
                build_command: None,
                output_dir: None,
                build_vars: None,
            })
            .await;
        let sources = output.content.find("COPY . .").unwrap();
        let workdir = output
            .content
            .find("WORKDIR /test_project/apps/web")
            .unwrap();
        let install = output.content.find("RUN node scripts/install.js").unwrap();
        assert!(sources < workdir && workdir < install);
    }

    #[tokio::test]
    async fn custom_workspace_install_receives_sources() {
        let repo = turbo_workspace();
        let output = NextJs
            .dockerfile(DockerfileConfig {
                root_local_path: repo.path(),
                local_path: repo.path(),
                project_slug: "test-project",
                use_buildkit: false,
                install_command: Some("node scripts/install.js"),
                build_command: None,
                output_dir: None,
                build_vars: None,
            })
            .await;
        assert!(
            output.content.find("COPY . .").unwrap()
                < output.content.find("RUN node scripts/install.js").unwrap()
        );
    }

    #[tokio::test]
    async fn turbo_workspace_with_install_script_copies_everything_before_install() {
        let repo = turbo_workspace();
        write(
            repo.path(),
            "packages/db/package.json",
            r#"{"name":"@repo/db","scripts":{"postinstall":"prisma generate"}}"#,
        );
        let dockerfile = generate(repo.path(), true).await;

        let copy_all = dockerfile.find("COPY . .\n").unwrap();
        let install = dockerfile.find("pnpm install").unwrap();
        assert!(copy_all < install, "{dockerfile}");
        assert!(
            dockerfile.contains("packages/db/package.json has a `postinstall` script"),
            "the Dockerfile should say why the install is not isolated:\n{dockerfile}"
        );
        // Sources are already in place; they are not copied a second time.
        assert!(
            !dockerfile.contains("COPY . /test_project/"),
            "{dockerfile}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fallback_reason_cannot_inject_dockerfile_instructions() {
        let repo = turbo_workspace();
        write(
            repo.path(),
            "packages/x\nRUN touch /pwned/package.json",
            r#"{"scripts":{"postinstall":"true"}}"#,
        );
        let dockerfile = generate(repo.path(), true).await;
        assert!(
            !dockerfile.lines().any(|line| line.starts_with("RUN touch")),
            "{dockerfile}"
        );
    }

    /// Parses a raw `curl -w "%{http_code}"` output and reports whether it
    /// represents a successful (2xx) response. A container that answers with
    /// 201/204/206 is not "down" -- only an exact-match on `"200"` would
    /// wrongly keep polling in those cases. Redirects (3xx) and error codes
    /// still count as not-ready, so the poll keeps waiting for them.
    fn curl_status_indicates_success(raw_status: &str) -> bool {
        raw_status
            .trim()
            .parse::<u16>()
            .is_ok_and(|code| (200..300).contains(&code))
    }

    #[test]
    fn curl_status_indicates_success_accepts_full_2xx_range() {
        assert!(curl_status_indicates_success("200"));
        assert!(curl_status_indicates_success("201"));
        assert!(curl_status_indicates_success("204"));
        assert!(curl_status_indicates_success("299"));
    }

    #[test]
    fn curl_status_indicates_success_rejects_non_2xx_and_malformed_output() {
        assert!(!curl_status_indicates_success("101"));
        assert!(!curl_status_indicates_success("301"));
        assert!(!curl_status_indicates_success("404"));
        assert!(!curl_status_indicates_success("500"));
        assert!(!curl_status_indicates_success(""));
        assert!(!curl_status_indicates_success(
            "curl: (7) Failed to connect"
        ));
    }

    /// Integration test that builds and runs a real Next.js Docker image
    /// This test requires Docker to be running and may take several minutes.
    /// It uses the fixture at tests/fixtures/nextjs-hello-world
    #[tokio::test]
    async fn test_nextjs_docker_build_and_run() {
        use std::process::Command;
        use std::time::Duration;

        // Check if Docker is available
        let docker_check = Command::new("docker").args(["info"]).output();

        if docker_check.is_err() || !docker_check.unwrap().status.success() {
            println!("Docker is not available, skipping test");
            return;
        }

        if !Command::new("docker")
            .args(["buildx", "version"])
            .output()
            .is_ok_and(|output| output.status.success())
        {
            println!("Docker buildx is not available, skipping BuildKit CLI test");
            return;
        }

        // Get the fixture path
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
        let fixture_path =
            std::path::PathBuf::from(&manifest_dir).join("tests/fixtures/nextjs-hello-world");

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
            .args([
                "-r",
                fixture_path.to_str().unwrap(),
                temp_dir.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to copy fixture");

        if !copy_result.status.success() {
            panic!(
                "Failed to copy fixture: {:?}",
                String::from_utf8_lossy(&copy_result.stderr)
            );
        }

        let project_dir = temp_dir.join("nextjs-hello-world");

        // Remove node_modules from copied fixture — Docker must install its own
        // (local node_modules may contain platform-specific binaries)
        let _ = std::fs::remove_dir_all(project_dir.join("node_modules"));

        // Write .dockerignore to prevent node_modules from leaking into build context
        std::fs::write(project_dir.join(".dockerignore"), "node_modules\n.next\n")
            .expect("Failed to write .dockerignore");

        // Generate Dockerfile using the preset
        let preset = NextJs;
        let dockerfile_result = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: &project_dir,
                local_path: &project_dir,
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "nextjs-test",
            })
            .await;

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
                "--no-cache",
                "-t",
                &image_name,
                "-f",
                dockerfile_path.to_str().unwrap(),
                project_dir.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to execute docker build");

        println!(
            "Build stdout:\n{}",
            String::from_utf8_lossy(&build_result.stdout)
        );
        println!(
            "Build stderr:\n{}",
            String::from_utf8_lossy(&build_result.stderr)
        );

        if !build_result.status.success() {
            let stderr = String::from_utf8_lossy(&build_result.stderr);
            // Skip gracefully on transient Docker/network errors (TLS, registry, timeout)
            if stderr.contains("failed to verify certificate")
                || stderr.contains("failed to resolve source metadata")
                || stderr.contains("timeout")
            {
                println!("Docker build failed due to transient network error, skipping test");
                std::fs::remove_dir_all(&temp_dir).ok();
                return;
            }
            // Cleanup temp directory
            std::fs::remove_dir_all(&temp_dir).ok();
            panic!("Docker build failed: {}", stderr);
        }

        // Run the container
        let container_name = format!("temps-nextjs-test-{}", test_id);

        // Clean up any stale containers from previous test runs
        let _ = Command::new("docker")
            .args(["rm", "-f", &container_name])
            .output();

        // Use a dynamic port to avoid conflicts
        let host_port = 30000 + (test_id % 10000) as u16;

        println!("Starting container: {}", container_name);

        let run_result = Command::new("docker")
            .args([
                "run",
                "-d",
                "--name",
                &container_name,
                "-p",
                &format!("{}:3000", host_port),
                &image_name,
            ])
            .output()
            .expect("Failed to execute docker run");

        if !run_result.status.success() {
            // Cleanup
            Command::new("docker")
                .args(["rmi", "-f", &image_name])
                .output()
                .ok();
            std::fs::remove_dir_all(&temp_dir).ok();
            panic!(
                "Docker run failed: {}",
                String::from_utf8_lossy(&run_result.stderr)
            );
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

            println!(
                "Attempt {}/{} - Logs: {} {}",
                attempts, max_attempts, logs, logs_stderr
            );

            // Check if Next.js is ready
            if logs.contains("Ready")
                || logs_stderr.contains("Ready")
                || logs.contains("started server")
                || logs_stderr.contains("started server")
            {
                is_healthy = true;
                break;
            }

            // Also try HTTP request
            let curl_result = Command::new("curl")
                .args([
                    "-s",
                    "-o",
                    "/dev/null",
                    "-w",
                    "%{http_code}",
                    &format!("http://localhost:{}", host_port),
                ])
                .output();

            if let Ok(output) = curl_result {
                let status = String::from_utf8_lossy(&output.stdout);
                if curl_status_indicates_success(&status) {
                    is_healthy = true;
                    println!("HTTP health check passed with status {status}");
                    break;
                }
            }
        }

        // Get final container logs for debugging
        let final_logs = Command::new("docker")
            .args(["logs", &container_name])
            .output()
            .expect("Failed to get final logs");

        println!(
            "Final container stdout:\n{}",
            String::from_utf8_lossy(&final_logs.stdout)
        );
        println!(
            "Final container stderr:\n{}",
            String::from_utf8_lossy(&final_logs.stderr)
        );

        // Check container status
        let inspect_result = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Status}}", &container_name])
            .output()
            .expect("Failed to inspect container");

        let container_status = String::from_utf8_lossy(&inspect_result.stdout)
            .trim()
            .to_string();
        println!("Container status: {}", container_status);

        // Cleanup: Stop and remove container, remove image
        println!("Cleaning up...");
        Command::new("docker")
            .args(["stop", &container_name])
            .output()
            .ok();
        Command::new("docker")
            .args(["rm", "-f", &container_name])
            .output()
            .ok();
        Command::new("docker")
            .args(["rmi", "-f", &image_name])
            .output()
            .ok();
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
