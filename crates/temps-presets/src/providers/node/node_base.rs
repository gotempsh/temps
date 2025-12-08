//! Base functionality for Node.js preset providers
//!
//! Provides shared logic for:
//! - Package manager detection and installation in Dockerfile
//! - Multi-stage Docker builds
//! - Common build patterns
//! - Security hardening using distroless images

use crate::providers::app::App;
use super::package_manager::PackageManager;

/// Google's distroless Node.js image - contains ONLY Node.js runtime.
/// No shell, no package manager, no OS utilities = maximum security.
/// See: https://github.com/GoogleContainerTools/distroless
const DISTROLESS_NODEJS: &str = "gcr.io/distroless/nodejs22-debian12:nonroot";

/// Security hardening for nginx Alpine runner
/// Note: nginx doesn't have a distroless variant, so we harden Alpine instead
const NGINX_SECURITY_HARDENING: &str = r#"# Security hardening - remove package manager and run as non-root
RUN rm -rf /sbin/apk /usr/bin/apk /etc/apk /var/cache/apk /lib/apk && \
    rm -rf /var/lib/apt /usr/bin/apt* /usr/bin/dpkg* 2>/dev/null || true && \
    chown -R nginx:nginx /usr/share/nginx/html /var/cache/nginx /var/log/nginx /etc/nginx/conf.d && \
    touch /var/run/nginx.pid && chown nginx:nginx /var/run/nginx.pid

USER nginx"#;

/// Configuration for generating a Node.js Dockerfile
#[derive(Debug, Clone)]
pub struct NodeDockerfileConfig {
    /// Install command (e.g., "npm ci", "pnpm install --frozen-lockfile")
    pub install_cmd: String,
    /// Build command (e.g., "npm run build")
    pub build_cmd: String,
    /// Start command for production
    pub start_cmd: String,
    /// Output directory (for static builds)
    pub output_dir: Option<String>,
    /// Port to expose
    pub port: u16,
    /// Whether this is a static build (uses nginx) or server build (uses Node runtime)
    pub is_static: bool,
    /// Additional environment variables for build stage
    pub build_env: Vec<(String, String)>,
    /// Whether this is a Next.js standalone build (uses server.js)
    pub is_nextjs_standalone: bool,
}

impl NodeDockerfileConfig {
    /// Create config from app with package manager detection
    pub fn from_app(
        app: &App,
        build_script: &str,
        start_cmd: String,
        output_dir: Option<String>,
        port: u16,
        is_static: bool,
    ) -> Self {
        let pm = PackageManager::detect(app);

        Self {
            install_cmd: pm.install_command().to_string(),
            build_cmd: pm.build_command(build_script),
            start_cmd,
            output_dir,
            port,
            is_static,
            build_env: Vec::new(),
            is_nextjs_standalone: false,
        }
    }
}

/// Generate multi-stage Dockerfile for Node.js applications
///
/// This generates a production-optimized Dockerfile with:
/// - Package manager installation in base stage
/// - Dependency installation in deps stage
/// - Build in builder stage
/// - Minimal runtime in final stage (nginx for static, node for server)
pub fn generate_node_dockerfile(app: &App, config: NodeDockerfileConfig) -> String {
    let pm = PackageManager::detect(app);
    let base_image = pm.base_image_for_app(app);

    // Determine package manager setup commands
    let pm_setup = match pm {
        PackageManager::Pnpm => {
            r#"# Install pnpm globally
RUN npm install -g pnpm@latest"#
        }
        PackageManager::Yarn1 | PackageManager::YarnBerry => {
            r#"# Enable corepack for Yarn
RUN corepack enable"#
        }
        PackageManager::Bun => {
            r#"# Install Bun on Node.js image
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:$PATH""#
        }
        PackageManager::Npm => {
            // npm is already included in node image
            ""
        }
    };

    // Build environment variables
    let build_env_lines = if config.build_env.is_empty() {
        String::new()
    } else {
        config
            .build_env
            .iter()
            .map(|(key, value)| format!("ENV {}=\"{}\"", key, value))
            .collect::<Vec<_>>()
            .join("\n")
    };

    if config.is_static {
        // Static build with nginx - hardened for security
        format!(
            r#"# syntax=docker/dockerfile:1

# Base image with package manager
FROM {base_image} AS base
{pm_setup}

# Install dependencies
FROM base AS deps
WORKDIR /app

# Copy all package manager files including Yarn Berry configuration
COPY package.json ./
COPY package-lock.json* ./
COPY yarn.lock* ./
COPY pnpm-lock.yaml* ./
COPY bun.lockb* ./
COPY .yarnrc.yml* ./
COPY .yarn* ./.yarn/

# Install dependencies
RUN {install_cmd}

# Build stage
FROM base AS builder
WORKDIR /app

# Copy dependencies
COPY --from=deps /app/node_modules ./node_modules

# Copy source code
COPY . .

# Set build environment
{build_env}

# Build application
RUN {build_cmd}

# Production stage with nginx (security hardened)
FROM nginx:alpine AS runner

# Copy built static files
COPY --from=builder /app/{output_dir} /usr/share/nginx/html

# Copy custom nginx config if it exists
COPY nginx.conf /etc/nginx/conf.d/default.conf 2>/dev/null || echo "No custom nginx config found, using default"

{nginx_hardening}

EXPOSE {port}

CMD ["nginx", "-g", "daemon off;"]
"#,
            base_image = base_image,
            pm_setup = pm_setup,
            install_cmd = config.install_cmd,
            build_env = build_env_lines,
            build_cmd = config.build_cmd,
            output_dir = config.output_dir.unwrap_or_else(|| "dist".to_string()),
            nginx_hardening = NGINX_SECURITY_HARDENING,
            port = config.port
        )
    } else {
        // Server build with Node.js runtime using Google's Distroless for maximum security
        // Distroless has NO shell, NO package manager, NO attack surface
        let is_standalone = config.is_nextjs_standalone || is_nextjs_standalone(&config.output_dir);
        let start_cmd_formatted = format_start_command_distroless(&config.start_cmd, is_standalone);

        format!(
            r#"# syntax=docker/dockerfile:1

# Base image with package manager (for building only)
FROM {base_image} AS base
{pm_setup}

# Install dependencies
FROM base AS deps
WORKDIR /app

# Copy all package manager files including Yarn Berry configuration
COPY package.json ./
COPY package-lock.json* ./
COPY yarn.lock* ./
COPY pnpm-lock.yaml* ./
COPY bun.lockb* ./
COPY .yarnrc.yml* ./
COPY .yarn* ./.yarn/

# Install dependencies
RUN {install_cmd}

# Build stage
FROM base AS builder
WORKDIR /app

# Copy dependencies
COPY --from=deps /app/node_modules ./node_modules

# Copy source code
COPY . .

# Set build environment
{build_env}

# Build application
RUN {build_cmd}

# Production stage using Google's Distroless image
# Distroless contains ONLY the Node.js runtime - no shell, no package manager, no attack surface
# This is the most secure option for running Node.js in production
# See: https://github.com/GoogleContainerTools/distroless
FROM {distroless} AS runner
WORKDIR /app

ENV NODE_ENV=production

# Distroless :nonroot tag already runs as non-root user (uid 65532)
# No RUN commands possible - distroless has no shell

{copy_output}

EXPOSE {port}

ENV PORT={port}
ENV HOSTNAME="0.0.0.0"
ENV HOST="0.0.0.0"

# Distroless nodejs images use node as entrypoint, so we just specify the script
CMD {start_cmd}
"#,
            base_image = base_image,
            install_cmd = config.install_cmd,
            build_env = build_env_lines,
            build_cmd = config.build_cmd,
            distroless = DISTROLESS_NODEJS,
            copy_output = generate_copy_output(&config.output_dir),
            port = config.port,
            start_cmd = start_cmd_formatted
        )
    }
}

/// Format start command for Distroless Docker CMD
/// Distroless nodejs images have `node` as the entrypoint, so we only pass args
fn format_start_command_distroless(cmd: &str, is_nextjs_standalone: bool) -> String {
    // Strip "node " prefix if present since distroless has node as entrypoint
    let cmd = cmd.strip_prefix("node ").unwrap_or(cmd);

    // For Next.js standalone builds, use server.js directly
    if is_nextjs_standalone {
        return "[\"server.js\"]".to_string();
    }

    // Handle npm/yarn/pnpm/npx start commands - these need node to run the script
    // For distroless, we need to specify the actual script path
    if cmd.starts_with("npm ") || cmd.starts_with("yarn ") || cmd.starts_with("pnpm ") || cmd.starts_with("npx ") {
        // For Next.js non-standalone, we run next start via the installed binary
        // The path is relative to node_modules which is copied to the image
        return "[\"./node_modules/next/dist/bin/next\", \"start\"]".to_string();
    }

    if cmd.contains(' ') {
        // Parse "server.js --port 3000" -> ["server.js", "--port", "3000"]
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        format!(
            "[{}]",
            parts
                .iter()
                .map(|s| format!("\"{}\"", s))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!("[\"{}\"]", cmd)
    }
}

/// Check if the output directory indicates a Next.js standalone build
fn is_nextjs_standalone(output_dir: &Option<String>) -> bool {
    output_dir.as_ref().is_some_and(|d| d.contains(".next/standalone"))
}

/// Generate the COPY instructions for the production stage
/// Handles special cases like Next.js standalone and non-standalone builds
fn generate_copy_output(output_dir: &Option<String>) -> String {
    if is_nextjs_standalone(output_dir) {
        // Next.js standalone builds require special handling:
        // 1. Copy the standalone directory contents (including server.js) to /app
        // 2. Copy the static files to .next/static (required for Next.js)
        // 3. Copy public files if they exist
        // See: https://nextjs.org/docs/app/api-reference/next-config-js/output#automatically-copying-traced-files
        r#"# Copy Next.js standalone build - server.js and dependencies
COPY --from=builder --chown=65532:65532 /app/.next/standalone ./
# Copy static assets (required for Next.js to serve static files)
COPY --from=builder --chown=65532:65532 /app/.next/static ./.next/static
# Copy public folder if it exists (Next.js requires this for public assets)
COPY --from=builder --chown=65532:65532 /app/public ./public"#.to_string()
    } else if output_dir.as_ref().is_some_and(|d| d == ".next") {
        // Next.js non-standalone build - needs node_modules to run `next start`
        // This follows the pattern from the distroless example:
        // https://github.com/vercel/next.js/tree/canary/examples/with-docker
        r#"# Copy Next.js config and dependencies for non-standalone build
COPY --from=builder --chown=65532:65532 /app/next.config.js* ./
COPY --from=builder --chown=65532:65532 /app/next.config.mjs* ./
COPY --from=builder --chown=65532:65532 /app/next.config.ts* ./
COPY --from=builder --chown=65532:65532 /app/package.json ./
# Copy node_modules (required for next start command)
COPY --from=builder --chown=65532:65532 /app/node_modules ./node_modules
# Copy built Next.js application
COPY --from=builder --chown=65532:65532 /app/.next ./.next
# Copy public folder if it exists
COPY --from=builder --chown=65532:65532 /app/public ./public"#.to_string()
    } else if let Some(ref dir) = output_dir {
        // Regular server build - copy the output directory
        format!(
            r#"# Copy necessary files (owned by nonroot user uid 65532)
COPY --from=builder --chown=65532:65532 /app/package.json ./
COPY --from=builder --chown=65532:65532 /app/node_modules ./node_modules
# Copy built application
COPY --from=builder --chown=65532:65532 /app/{} ./{}"#,
            dir, dir
        )
    } else {
        // Default: copy dist folder
        r#"# Copy necessary files (owned by nonroot user uid 65532)
COPY --from=builder --chown=65532:65532 /app/package.json ./
COPY --from=builder --chown=65532:65532 /app/node_modules ./node_modules
# Copy built application
COPY --from=builder --chown=65532:65532 /app/dist ./dist"#.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_format_start_command_distroless() {
        // Distroless strips "node " since node is the entrypoint
        assert_eq!(format_start_command_distroless("node server.js", false), "[\"server.js\"]");
        assert_eq!(format_start_command_distroless("server.js", false), "[\"server.js\"]");
        assert_eq!(format_start_command_distroless("node server.js --port 3000", false), "[\"server.js\", \"--port\", \"3000\"]");
    }

    #[test]
    fn test_format_start_command_distroless_nextjs_standalone() {
        // Next.js standalone mode uses server.js directly
        assert_eq!(format_start_command_distroless("node server.js", true), "[\"server.js\"]");
        assert_eq!(format_start_command_distroless("npx next start", true), "[\"server.js\"]");
    }

    #[test]
    fn test_format_start_command_distroless_nextjs_non_standalone() {
        // Next.js non-standalone mode uses next start via node_modules
        assert_eq!(
            format_start_command_distroless("npx next start", false),
            "[\"./node_modules/next/dist/bin/next\", \"start\"]"
        );
        assert_eq!(
            format_start_command_distroless("npm run start", false),
            "[\"./node_modules/next/dist/bin/next\", \"start\"]"
        );
    }

    #[test]
    fn test_generate_static_dockerfile() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("package-lock.json".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "npm start".to_string(),
            Some("dist".to_string()),
            80,
            true,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        assert!(dockerfile.contains("FROM node:22-alpine AS base"));
        assert!(dockerfile.contains("FROM nginx:alpine AS runner"));
        assert!(dockerfile.contains("npm ci"));
        assert!(dockerfile.contains("npm run build"));
        assert!(dockerfile.contains("EXPOSE 80"));
    }

    #[test]
    fn test_generate_static_dockerfile_security_hardening() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("package-lock.json".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "npm start".to_string(),
            Some("dist".to_string()),
            80,
            true,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        // Security: Package manager removal (all in one rm command)
        assert!(
            dockerfile.contains("/sbin/apk") && dockerfile.contains("/usr/bin/apk"),
            "Should remove apk package manager binaries"
        );
        assert!(
            dockerfile.contains("/etc/apk"),
            "Should remove apk config directory"
        );

        // Security: Non-root user for nginx
        assert!(
            dockerfile.contains("USER nginx"),
            "nginx should run as non-root user"
        );

        // Security: Proper permissions for nginx
        assert!(
            dockerfile.contains("chown -R nginx:nginx"),
            "Should set nginx ownership"
        );
    }

    #[test]
    fn test_generate_server_dockerfile_uses_distroless() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("pnpm-lock.yaml".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "node server.js".to_string(),
            Some(".next".to_string()),
            3000,
            false,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        // Build stage uses regular Node.js Alpine
        assert!(dockerfile.contains("FROM node:22-alpine AS base"));
        assert!(dockerfile.contains("npm install -g pnpm@latest"));
        assert!(dockerfile.contains("pnpm install --frozen-lockfile"));
        assert!(dockerfile.contains("pnpm run build"));

        // Production stage uses distroless
        assert!(
            dockerfile.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless Node.js image for production"
        );

        // Distroless strips "node" from command since it's the entrypoint
        assert!(dockerfile.contains("CMD [\"server.js\"]"));
        assert!(dockerfile.contains("EXPOSE 3000"));
    }

    #[test]
    fn test_generate_server_dockerfile_distroless_security() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("package-lock.json".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "node server.js".to_string(),
            Some(".next".to_string()),
            3000,
            false,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        // Security: Uses distroless (no shell, no package manager)
        assert!(
            dockerfile.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless image"
        );

        // Security: Distroless nonroot uses uid 65532
        assert!(
            dockerfile.contains("--chown=65532:65532"),
            "Should copy files with distroless nonroot user ownership"
        );

        // Security: No RUN commands in production stage (distroless has no shell)
        let production_stage = dockerfile.split("FROM gcr.io/distroless").nth(1).unwrap_or("");
        assert!(
            !production_stage.contains("\nRUN "),
            "Distroless stage should not have RUN commands (no shell available)"
        );
    }

    #[test]
    fn test_generate_dockerfile_with_bun_uses_distroless() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("bun.lockb".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "node server.js".to_string(),
            Some("dist".to_string()),
            3000,
            false,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        // Build stage uses Node.js image with Bun installed
        assert!(dockerfile.contains("FROM node:22-alpine AS base"));
        assert!(dockerfile.contains("RUN curl -fsSL https://bun.sh/install | bash"));
        assert!(dockerfile.contains("ENV PATH=\"/root/.bun/bin:$PATH\""));
        assert!(dockerfile.contains("bun install"));
        assert!(dockerfile.contains("bun run build"));

        // Production stage uses distroless
        assert!(
            dockerfile.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Bun Dockerfile should also use distroless for production"
        );

        // Distroless nonroot user
        assert!(
            dockerfile.contains("--chown=65532:65532"),
            "Bun Dockerfile should use distroless nonroot ownership"
        );
    }

    #[test]
    fn test_distroless_constant() {
        // Verify the distroless constant is set correctly
        assert!(
            DISTROLESS_NODEJS.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Distroless constant should point to nonroot nodejs image"
        );
    }

    #[test]
    fn test_nginx_hardening_constants() {
        // Verify nginx hardening (nginx doesn't have distroless variant)
        assert!(
            NGINX_SECURITY_HARDENING.contains("USER nginx"),
            "Nginx hardening should set user"
        );
        assert!(
            NGINX_SECURITY_HARDENING.contains("chown -R nginx:nginx"),
            "Nginx hardening should set ownership"
        );
        assert!(
            NGINX_SECURITY_HARDENING.contains("rm -rf /sbin/apk"),
            "Nginx hardening should remove package manager"
        );
    }

    #[test]
    fn test_generate_nextjs_non_standalone_dockerfile() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), r#"{"dependencies":{"next":"14.0.0"}}"#.to_string());
        files.insert("package-lock.json".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        // Simulate Next.js non-standalone config
        let config = NodeDockerfileConfig {
            install_cmd: "npm ci".to_string(),
            build_cmd: "npm run build".to_string(),
            start_cmd: "npx next start".to_string(),
            output_dir: Some(".next".to_string()), // Non-standalone uses .next
            port: 3000,
            is_static: false,
            build_env: Vec::new(),
            is_nextjs_standalone: false,
        };

        let dockerfile = generate_node_dockerfile(&app, config);

        // Should use distroless
        assert!(
            dockerfile.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless image"
        );

        // Should copy node_modules for next start
        assert!(
            dockerfile.contains("COPY --from=builder --chown=65532:65532 /app/node_modules ./node_modules"),
            "Should copy node_modules for non-standalone Next.js"
        );

        // Should copy .next directory
        assert!(
            dockerfile.contains("COPY --from=builder --chown=65532:65532 /app/.next ./.next"),
            "Should copy .next directory"
        );

        // Should use next start via node_modules
        assert!(
            dockerfile.contains("CMD [\"./node_modules/next/dist/bin/next\", \"start\"]"),
            "Should run next start via node_modules path"
        );
    }

    #[test]
    fn test_generate_nextjs_standalone_dockerfile() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), r#"{"dependencies":{"next":"14.0.0"}}"#.to_string());
        files.insert("package-lock.json".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        // Simulate Next.js standalone config
        let config = NodeDockerfileConfig {
            install_cmd: "npm ci".to_string(),
            build_cmd: "npm run build".to_string(),
            start_cmd: "node server.js".to_string(),
            output_dir: Some(".next/standalone".to_string()), // Standalone output
            port: 3000,
            is_static: false,
            build_env: Vec::new(),
            is_nextjs_standalone: true,
        };

        let dockerfile = generate_node_dockerfile(&app, config);

        // Should use distroless
        assert!(
            dockerfile.contains("gcr.io/distroless/nodejs22-debian12:nonroot"),
            "Should use distroless image"
        );

        // Should copy standalone directory
        assert!(
            dockerfile.contains("COPY --from=builder --chown=65532:65532 /app/.next/standalone ./"),
            "Should copy standalone directory"
        );

        // Should copy static files
        assert!(
            dockerfile.contains("COPY --from=builder --chown=65532:65532 /app/.next/static ./.next/static"),
            "Should copy static files"
        );

        // Should run server.js directly
        assert!(
            dockerfile.contains("CMD [\"server.js\"]"),
            "Should run server.js directly for standalone build"
        );
    }
}
