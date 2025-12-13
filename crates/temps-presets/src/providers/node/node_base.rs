//! Base functionality for Node.js preset providers
//!
//! Provides shared logic for:
//! - Package manager detection and installation in Dockerfile
//! - Multi-stage Docker builds
//! - Common build patterns
//! - Security hardening using Alpine images with package manager removal

use crate::providers::app::App;
use super::package_manager::PackageManager;

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
        // Server build with hardened Alpine Node.js runner
        // Secure: removes package managers and runs as non-root
        // Benefits over distroless:
        // - Full CA certificates for HTTPS fetch calls
        // - Shell access for debugging if needed
        let is_standalone = config.is_nextjs_standalone || is_nextjs_standalone(&config.output_dir);
        let start_cmd_formatted = format_start_command_alpine(&config.start_cmd, is_standalone);

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

# Production stage using hardened Alpine Node.js
# Secure: non-root user, package manager removed, full CA certificates
FROM node:22-alpine AS runner
WORKDIR /app

ENV NODE_ENV=production

{alpine_hardening}

{copy_output}

EXPOSE {port}

ENV PORT={port}
ENV HOSTNAME="0.0.0.0"
ENV HOST="0.0.0.0"

CMD {start_cmd}
"#,
            base_image = base_image,
            install_cmd = config.install_cmd,
            build_env = build_env_lines,
            build_cmd = config.build_cmd,
            alpine_hardening = NODEJS_ALPINE_SECURITY_HARDENING,
            copy_output = generate_copy_output_alpine(&config.output_dir),
            port = config.port,
            start_cmd = start_cmd_formatted
        )
    }
}

/// Check if the output directory indicates a Next.js standalone build
fn is_nextjs_standalone(output_dir: &Option<String>) -> bool {
    output_dir.as_ref().is_some_and(|d| d.contains(".next/standalone"))
}

/// Format start command for Alpine Docker CMD (standard shell form)
/// Alpine has a shell, so we can use standard command syntax
fn format_start_command_alpine(cmd: &str, is_nextjs_standalone: bool) -> String {
    // For Next.js standalone builds, use node server.js
    if is_nextjs_standalone {
        return "[\"node\", \"server.js\"]".to_string();
    }

    // Handle npm/yarn/pnpm/npx start commands
    if cmd.starts_with("npm ") || cmd.starts_with("yarn ") || cmd.starts_with("pnpm ") || cmd.starts_with("npx ") {
        // For Next.js non-standalone, run next start via node_modules
        return "[\"node\", \"./node_modules/next/dist/bin/next\", \"start\"]".to_string();
    }

    // Convert command to exec form
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    format!(
        "[{}]",
        parts
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Generate the COPY instructions for Alpine production stage
/// Uses nodejs:nodejs user (uid 1001) instead of distroless nonroot (uid 65532)
fn generate_copy_output_alpine(output_dir: &Option<String>) -> String {
    if is_nextjs_standalone(output_dir) {
        // Next.js standalone builds require special handling
        r#"# Copy Next.js standalone build - server.js and dependencies
COPY --from=builder --chown=nodejs:nodejs /app/.next/standalone ./
# Copy static assets (required for Next.js to serve static files)
COPY --from=builder --chown=nodejs:nodejs /app/.next/static ./.next/static
# Copy public folder if it exists (Next.js requires this for public assets)
COPY --from=builder --chown=nodejs:nodejs /app/public ./public"#.to_string()
    } else if output_dir.as_ref().is_some_and(|d| d == ".next") {
        // Next.js non-standalone build - needs node_modules to run `next start`
        r#"# Copy Next.js config and dependencies for non-standalone build
COPY --from=builder --chown=nodejs:nodejs /app/next.config.js* ./
COPY --from=builder --chown=nodejs:nodejs /app/next.config.mjs* ./
COPY --from=builder --chown=nodejs:nodejs /app/next.config.ts* ./
COPY --from=builder --chown=nodejs:nodejs /app/package.json ./
# Copy node_modules (required for next start command)
COPY --from=builder --chown=nodejs:nodejs /app/node_modules ./node_modules
# Copy built Next.js application
COPY --from=builder --chown=nodejs:nodejs /app/.next ./.next
# Copy public folder if it exists
COPY --from=builder --chown=nodejs:nodejs /app/public ./public"#.to_string()
    } else if let Some(ref dir) = output_dir {
        // Regular server build - copy the output directory
        format!(
            r#"# Copy necessary files (owned by nodejs user)
COPY --from=builder --chown=nodejs:nodejs /app/package.json ./
COPY --from=builder --chown=nodejs:nodejs /app/node_modules ./node_modules
# Copy built application
COPY --from=builder --chown=nodejs:nodejs /app/{} ./{}"#,
            dir, dir
        )
    } else {
        // Default: copy dist folder
        r#"# Copy necessary files (owned by nodejs user)
COPY --from=builder --chown=nodejs:nodejs /app/package.json ./
COPY --from=builder --chown=nodejs:nodejs /app/node_modules ./node_modules
# Copy built application
COPY --from=builder --chown=nodejs:nodejs /app/dist ./dist"#.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_format_start_command_alpine() {
        // Alpine uses node explicitly in the command
        assert_eq!(format_start_command_alpine("node server.js", false), "[\"node\", \"server.js\"]");
        assert_eq!(format_start_command_alpine("server.js", false), "[\"server.js\"]");
        assert_eq!(format_start_command_alpine("node server.js --port 3000", false), "[\"node\", \"server.js\", \"--port\", \"3000\"]");
    }

    #[test]
    fn test_format_start_command_alpine_nextjs_standalone() {
        // Next.js standalone mode uses node server.js
        assert_eq!(format_start_command_alpine("node server.js", true), "[\"node\", \"server.js\"]");
        assert_eq!(format_start_command_alpine("npx next start", true), "[\"node\", \"server.js\"]");
    }

    #[test]
    fn test_format_start_command_alpine_nextjs_non_standalone() {
        // Next.js non-standalone mode uses next start via node_modules
        assert_eq!(
            format_start_command_alpine("npx next start", false),
            "[\"node\", \"./node_modules/next/dist/bin/next\", \"start\"]"
        );
        assert_eq!(
            format_start_command_alpine("npm run start", false),
            "[\"node\", \"./node_modules/next/dist/bin/next\", \"start\"]"
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
    fn test_generate_server_dockerfile_uses_alpine() {
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

        // Production stage uses hardened Alpine
        assert!(
            dockerfile.contains("FROM node:22-alpine AS runner"),
            "Should use Node.js Alpine image for production"
        );

        // Alpine uses node explicitly in the command
        assert!(dockerfile.contains("CMD [\"node\""));
        assert!(dockerfile.contains("EXPOSE 3000"));
    }

    #[test]
    fn test_generate_server_dockerfile_alpine_security() {
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

        // Security: Uses hardened Alpine
        assert!(
            dockerfile.contains("FROM node:22-alpine AS runner"),
            "Should use Alpine image"
        );

        // Security: Creates non-root user with UID 1001
        assert!(
            dockerfile.contains("adduser --system --uid 1001 nodejs"),
            "Should create nodejs user with UID 1001"
        );

        // Security: Files owned by nodejs user
        assert!(
            dockerfile.contains("--chown=nodejs:nodejs"),
            "Should copy files with nodejs user ownership"
        );

        // Security: Runs as non-root user
        assert!(
            dockerfile.contains("USER nodejs"),
            "Should run as nodejs user"
        );

        // Security: Package manager removal
        assert!(
            dockerfile.contains("rm -rf /sbin/apk"),
            "Should remove apk package manager"
        );
    }

    #[test]
    fn test_generate_dockerfile_with_bun_uses_alpine() {
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

        // Production stage uses hardened Alpine
        assert!(
            dockerfile.contains("FROM node:22-alpine AS runner"),
            "Bun Dockerfile should use Alpine for production"
        );

        // Nodejs user ownership
        assert!(
            dockerfile.contains("--chown=nodejs:nodejs"),
            "Bun Dockerfile should use nodejs user ownership"
        );
    }

    #[test]
    fn test_alpine_hardening_constants() {
        // Verify Alpine hardening for Node.js
        assert!(
            NODEJS_ALPINE_SECURITY_HARDENING.contains("USER nodejs"),
            "Alpine hardening should set nodejs user"
        );
        assert!(
            NODEJS_ALPINE_SECURITY_HARDENING.contains("adduser --system --uid 1001 nodejs"),
            "Alpine hardening should create nodejs user with UID 1001"
        );
        assert!(
            NODEJS_ALPINE_SECURITY_HARDENING.contains("rm -rf /sbin/apk"),
            "Alpine hardening should remove package manager"
        );
    }

    #[test]
    fn test_nginx_hardening_constants() {
        // Verify nginx hardening
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

        // Should use hardened Alpine
        assert!(
            dockerfile.contains("FROM node:22-alpine AS runner"),
            "Should use Alpine image"
        );

        // Should copy node_modules for next start
        assert!(
            dockerfile.contains("COPY --from=builder --chown=nodejs:nodejs /app/node_modules ./node_modules"),
            "Should copy node_modules for non-standalone Next.js"
        );

        // Should copy .next directory
        assert!(
            dockerfile.contains("COPY --from=builder --chown=nodejs:nodejs /app/.next ./.next"),
            "Should copy .next directory"
        );

        // Should use next start via node_modules with node
        assert!(
            dockerfile.contains("CMD [\"node\", \"./node_modules/next/dist/bin/next\", \"start\"]"),
            "Should run next start via node_modules path with node"
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

        // Should use hardened Alpine
        assert!(
            dockerfile.contains("FROM node:22-alpine AS runner"),
            "Should use Alpine image"
        );

        // Should copy standalone directory
        assert!(
            dockerfile.contains("COPY --from=builder --chown=nodejs:nodejs /app/.next/standalone ./"),
            "Should copy standalone directory"
        );

        // Should copy static files
        assert!(
            dockerfile.contains("COPY --from=builder --chown=nodejs:nodejs /app/.next/static ./.next/static"),
            "Should copy static files"
        );

        // Should run node server.js
        assert!(
            dockerfile.contains("CMD [\"node\", \"server.js\"]"),
            "Should run node server.js for standalone build"
        );
    }
}
