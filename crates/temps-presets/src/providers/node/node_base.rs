//! Base functionality for Node.js preset providers
//!
//! Provides shared logic for:
//! - Package manager detection and installation in Dockerfile
//! - Multi-stage Docker builds
//! - Common build patterns

use crate::providers::app::App;
use super::package_manager::PackageManager;

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
        // Static build with nginx
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

# Production stage with nginx
FROM nginx:alpine AS runner

# Copy built static files
COPY --from=builder /app/{output_dir} /usr/share/nginx/html

# Copy custom nginx config if it exists
COPY nginx.conf /etc/nginx/conf.d/default.conf 2>/dev/null || echo "No custom nginx config found, using default"

EXPOSE {port}

CMD ["nginx", "-g", "daemon off;"]
"#,
            base_image = base_image,
            pm_setup = pm_setup,
            install_cmd = config.install_cmd,
            build_env = build_env_lines,
            build_cmd = config.build_cmd,
            output_dir = config.output_dir.unwrap_or_else(|| "dist".to_string()),
            port = config.port
        )
    } else {
        // Server build with Node.js runtime
        let start_cmd_formatted = format_start_command(&config.start_cmd);

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

# Production stage
FROM {base_image} AS runner
WORKDIR /app

ENV NODE_ENV=production
{pm_setup}

# Create non-root user
RUN addgroup --system --gid 1001 nodejs && \
    adduser --system --uid 1001 appuser

# Copy necessary files
COPY --from=builder --chown=appuser:nodejs /app/package.json ./
COPY --from=builder --chown=appuser:nodejs /app/node_modules ./node_modules

# Copy built application
{copy_output}

USER appuser

EXPOSE {port}

ENV PORT={port}
ENV HOSTNAME="0.0.0.0"
ENV HOST="0.0.0.0"

CMD {start_cmd}
"#,
            base_image = base_image,
            pm_setup = pm_setup,
            install_cmd = config.install_cmd,
            build_env = build_env_lines,
            build_cmd = config.build_cmd,
            copy_output = if let Some(ref output_dir) = config.output_dir {
                format!("COPY --from=builder --chown=appuser:nodejs /app/{} ./{}", output_dir, output_dir)
            } else {
                "COPY --from=builder --chown=appuser:nodejs /app/dist ./dist".to_string()
            },
            port = config.port,
            start_cmd = start_cmd_formatted
        )
    }
}

/// Format start command for Docker CMD
fn format_start_command(cmd: &str) -> String {
    if cmd.contains(' ') {
        // Parse "node server.js" -> ["node", "server.js"]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_format_start_command() {
        assert_eq!(format_start_command("npm start"), "[\"npm\", \"start\"]");
        assert_eq!(format_start_command("node server.js"), "[\"node\", \"server.js\"]");
        assert_eq!(format_start_command("start"), "[\"start\"]");
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
    fn test_generate_server_dockerfile() {
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

        assert!(dockerfile.contains("FROM node:22-alpine AS base"));
        assert!(dockerfile.contains("npm install -g pnpm@latest"));
        assert!(dockerfile.contains("pnpm install --frozen-lockfile"));
        assert!(dockerfile.contains("pnpm run build"));
        assert!(dockerfile.contains("CMD [\"node\", \"server.js\"]"));
        assert!(dockerfile.contains("EXPOSE 3000"));
    }

    #[test]
    fn test_generate_dockerfile_with_bun() {
        let mut files = HashMap::new();
        files.insert("package.json".to_string(), "{}".to_string());
        files.insert("bun.lockb".to_string(), "".to_string());

        let app = App::from_tree(PathBuf::from("/test"), files);

        let config = NodeDockerfileConfig::from_app(
            &app,
            "build",
            "bun run start".to_string(),
            Some("dist".to_string()),
            3000,
            false,
        );

        let dockerfile = generate_node_dockerfile(&app, config);

        // Bun now uses Node.js image with Bun installed on top
        assert!(dockerfile.contains("FROM node:22-alpine AS base"));
        assert!(dockerfile.contains("RUN curl -fsSL https://bun.sh/install | bash"));
        assert!(dockerfile.contains("ENV PATH=\"/root/.bun/bin:$PATH\""));
        assert!(dockerfile.contains("bun install"));
        assert!(dockerfile.contains("bun run build"));
    }
}
