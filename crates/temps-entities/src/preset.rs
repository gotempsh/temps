// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preset type definitions and configurations
//!
//! Type-safe preset identifiers that map to framework providers
//! Each preset has its own configuration struct that defines what settings it supports

use sea_orm::{DeriveActiveEnum, EnumIter, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Type-safe preset identifiers
///
/// Each preset maps to a specific framework provider implementation that determines:
/// - How to detect the framework
/// - Build/install/start commands
/// - Package manager detection
/// - Dockerfile generation
/// - Default configuration
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    // Node.js / TypeScript frameworks
    #[serde(rename = "nextjs")]
    #[sea_orm(string_value = "nextjs")]
    NextJs,

    #[sea_orm(string_value = "vite")]
    Vite,

    #[sea_orm(string_value = "astro")]
    Astro,

    #[sea_orm(string_value = "nuxt")]
    Nuxt,

    #[sea_orm(string_value = "remix")]
    Remix,

    #[serde(rename = "sveltekit")]
    #[sea_orm(string_value = "sveltekit")]
    SvelteKit,

    #[serde(rename = "solidstart")]
    #[sea_orm(string_value = "solidstart")]
    SolidStart,

    #[sea_orm(string_value = "angular")]
    Angular,

    #[sea_orm(string_value = "vue")]
    Vue,

    #[sea_orm(string_value = "react")]
    React,

    #[sea_orm(string_value = "docusaurus")]
    Docusaurus,

    #[sea_orm(string_value = "rsbuild")]
    Rsbuild,

    // Python frameworks
    #[sea_orm(string_value = "python")]
    Python,

    #[serde(rename = "fastapi")]
    #[sea_orm(string_value = "fastapi")]
    FastApi,

    #[sea_orm(string_value = "flask")]
    Flask,

    #[sea_orm(string_value = "django")]
    Django,

    // Ruby frameworks
    #[sea_orm(string_value = "rails")]
    Rails,

    // Go frameworks
    #[sea_orm(string_value = "go")]
    Go,

    // Rust frameworks
    #[sea_orm(string_value = "rust")]
    Rust,

    // Java frameworks
    #[sea_orm(string_value = "java")]
    Java,

    // PHP frameworks
    #[sea_orm(string_value = "laravel")]
    Laravel,

    // Generic presets
    #[sea_orm(string_value = "dockerfile")]
    Dockerfile,

    #[sea_orm(string_value = "nixpacks")]
    Nixpacks,

    /// Auto-detecting builder backed by the autopack crates.
    #[sea_orm(string_value = "autopack")]
    Autopack,

    #[sea_orm(string_value = "static")]
    Static,

    // Docker Compose
    #[serde(rename = "docker-compose")]
    #[sea_orm(string_value = "docker-compose")]
    DockerCompose,

    // Node.js runtime (for custom node apps)
    #[serde(rename = "nodejs")]
    #[sea_orm(string_value = "nodejs")]
    NodeJs,
}

impl Preset {
    /// Get the canonical persisted preset name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Preset::NextJs => "nextjs",
            Preset::Vite => "vite",
            Preset::Astro => "astro",
            Preset::Nuxt => "nuxt",
            Preset::Remix => "remix",
            Preset::SvelteKit => "sveltekit",
            Preset::SolidStart => "solidstart",
            Preset::Angular => "angular",
            Preset::Vue => "vue",
            Preset::React => "react",
            Preset::Docusaurus => "docusaurus",
            Preset::Rsbuild => "rsbuild",
            Preset::Python => "python",
            Preset::FastApi => "fastapi",
            Preset::Flask => "flask",
            Preset::Django => "django",
            Preset::Rails => "rails",
            Preset::Go => "go",
            Preset::Rust => "rust",
            Preset::Java => "java",
            Preset::Laravel => "laravel",
            Preset::Dockerfile => "dockerfile",
            Preset::DockerCompose => "docker-compose",
            Preset::Nixpacks => "nixpacks",
            Preset::Autopack => "autopack",
            Preset::Static => "static",
            Preset::NodeJs => "nodejs",
        }
    }

    /// List all available presets
    pub fn all() -> Vec<Preset> {
        vec![
            Preset::NextJs,
            Preset::Vite,
            Preset::Astro,
            Preset::Nuxt,
            Preset::Remix,
            Preset::SvelteKit,
            Preset::SolidStart,
            Preset::Angular,
            Preset::Vue,
            Preset::React,
            Preset::Docusaurus,
            Preset::Rsbuild,
            Preset::Python,
            Preset::FastApi,
            Preset::Flask,
            Preset::Django,
            Preset::Rails,
            Preset::Go,
            Preset::Rust,
            Preset::Java,
            Preset::Laravel,
            Preset::Dockerfile,
            Preset::DockerCompose,
            Preset::Nixpacks,
            Preset::Static,
            Preset::NodeJs,
        ]
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for Preset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "nextjs" => Ok(Preset::NextJs),
            "vite" => Ok(Preset::Vite),
            "astro" => Ok(Preset::Astro),
            "nuxt" => Ok(Preset::Nuxt),
            "remix" => Ok(Preset::Remix),
            "sveltekit" => Ok(Preset::SvelteKit),
            "solidstart" => Ok(Preset::SolidStart),
            "angular" => Ok(Preset::Angular),
            "vue" => Ok(Preset::Vue),
            "react" => Ok(Preset::React),
            "docusaurus" => Ok(Preset::Docusaurus),
            "rsbuild" => Ok(Preset::Rsbuild),
            "python" => Ok(Preset::Python),
            "fastapi" => Ok(Preset::FastApi),
            "flask" => Ok(Preset::Flask),
            "django" => Ok(Preset::Django),
            "rails" => Ok(Preset::Rails),
            "go" => Ok(Preset::Go),
            "rust" => Ok(Preset::Rust),
            "java" => Ok(Preset::Java),
            "laravel" => Ok(Preset::Laravel),
            "dockerfile" => Ok(Preset::Dockerfile),
            "docker-compose" | "dockercompose" | "compose" => Ok(Preset::DockerCompose),
            "nixpacks" => Ok(Preset::Nixpacks),
            "autopack" => Ok(Preset::Autopack),
            "static" => Ok(Preset::Static),
            "nodejs" | "node" => Ok(Preset::NodeJs),
            _ => Err(format!("Unknown preset: {}", s)),
        }
    }
}

// ============================================================================
// Preset Configuration Structs
// ============================================================================
//
// Each preset defines its own configuration struct that specifies what
// settings it supports. These are stored as JSONB in the database and
// deserialized when needed.
//
// All configuration structs should:
// - Derive Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema
// - Use Option<T> for optional fields
// - Use #[serde(skip_serializing_if = "Option::is_none")] for optional fields
// - Provide a Default implementation

/// Next.js preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NextJsConfig {
    /// Custom install command (default: auto-detected from package manager)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom start command (default: "npm run start")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,

    /// Output directory (default: ".next")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Vite preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ViteConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "dist")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Astro preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AstroConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "dist")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Nuxt preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NuxtConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom start command (default: "npm run start")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,

    /// Output directory (default: ".output")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Remix preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemixConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom start command (default: "npm run start")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,
}

/// SvelteKit preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SvelteKitConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// SolidStart preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SolidStartConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
}

/// Angular preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AngularConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "dist")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Vue preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct VueConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "dist")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// React preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReactConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Docusaurus preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DocusaurusConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Rsbuild preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RsbuildConfig {
    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command (default: "npm run build")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory (default: "dist")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Python preset configuration (generic)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct PythonConfig {
    /// Python version (default: "3.11")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,

    /// Requirements file path (default: "requirements.txt")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements_file: Option<String>,

    /// Main application file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_file: Option<String>,
}

/// FastAPI preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FastApiConfig {
    /// Python version (default: "3.11")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,

    /// Requirements file path (default: "requirements.txt")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements_file: Option<String>,

    /// Main application module (default: "main:app")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_module: Option<String>,
}

/// Flask preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlaskConfig {
    /// Python version (default: "3.11")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,

    /// Requirements file path (default: "requirements.txt")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements_file: Option<String>,

    /// Main application file (default: "app.py")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_file: Option<String>,
}

/// Django preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DjangoConfig {
    /// Python version (default: "3.11")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,

    /// Requirements file path (default: "requirements.txt")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements_file: Option<String>,

    /// Django settings module
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_module: Option<String>,
}

/// Rails preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RailsConfig {
    /// Ruby version (default: auto-detected from .ruby-version)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruby_version: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
}

/// Go preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct GoConfig {
    /// Go version (default: "1.21")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub go_version: Option<String>,

    /// Main package path (default: ".")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_package: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
}

/// Rust preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RustConfig {
    /// Rust version/channel (default: "stable")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust_version: Option<String>,

    /// Binary name to build (default: package name from Cargo.toml)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_name: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
}

/// Java preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct JavaConfig {
    /// Java version (default: "17")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub java_version: Option<String>,

    /// Build tool (maven or gradle, default: auto-detected)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_tool: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Main class or JAR file to run
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_class: Option<String>,
}

/// Laravel preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaravelConfig {
    /// PHP version (default: "8.2")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php_version: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
}

/// Catalog variant persisted under the canonical Dockerfile preset.
///
/// Existing rows predate this discriminator and therefore deserialize as
/// [`DockerfileVariant::File`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum DockerfileVariant {
    #[default]
    File,
    Custom,
    #[serde(rename = "workflow-worker")]
    WorkflowWorker,
}

impl DockerfileVariant {
    fn is_file(&self) -> bool {
        *self == Self::File
    }
}

/// Dockerfile preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DockerfileConfig {
    /// Catalog variant. Omitted for the standard user-provided Dockerfile flow.
    #[serde(default, skip_serializing_if = "DockerfileVariant::is_file")]
    pub variant: DockerfileVariant,

    /// Path to Dockerfile (default: "Dockerfile")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,

    /// Docker build context (default: ".")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_context: Option<String>,

    /// Docker build target stage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Container startup overrides, persisted with this project's settings.
    #[serde(flatten)]
    pub runtime: ContainerRuntimeOverrides,

    /// Runtime configuration for a curated service template that deploys a
    /// prebuilt image. This is intentionally stored with the project rather
    /// than inferred from the latest deployment so a version change is durable
    /// and applies to every subsequent environment deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_runtime: Option<ImageRuntimeConfig>,
}

/// Exec-form container startup overrides. Arguments are passed verbatim without
/// shell parsing; use an explicit shell entrypoint when shell syntax is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct ContainerRuntimeOverrides {
    /// Override image CMD. On creation, omitted/null uses the image default.
    /// On partial project updates, omission preserves the stored override;
    /// send null to restore the image default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Override image ENTRYPOINT. On creation, omitted/null inherits; [] clears it.
    /// On partial project updates, omission preserves the stored override;
    /// send null to restore the image default, or [] to clear the entrypoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
}

/// Editable runtime settings for a single-container image template.
///
/// Multi-container service templates will use a separate container collection;
/// keeping this shape explicitly singular prevents silently applying one image
/// or command to an unrelated sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageRuntimeConfig {
    pub image_ref: String,
    /// `None` explicitly means "use the image's default command". Keep the
    /// serialized `null` when a runtime snapshot exists so clients can
    /// distinguish that choice from an omitted runtime setting.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
}

/// Docker Compose preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DockerComposeConfig {
    /// Path to compose file relative to project directory (default: "docker-compose.yml")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_path: Option<String>,
    /// User-provided docker-compose.override.yml content.
    /// Merged with the main compose file at deploy time.
    /// Use to override ports, volumes, environment, commands, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_override: Option<String>,
    /// Ports to expose publicly through the proxy.
    /// Each entry specifies a service name and container port that gets a public subdomain.
    /// All other ports remain private (accessible only via host-mapped ports).
    /// Format: `[{"service": "web", "port": 8080}, {"service": "api", "port": 3000}]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_ports: Vec<ComposePublicPort>,
    /// Compose service names to exclude from deployment entirely (and strip
    /// from other services' `depends_on`) — e.g. a `postgres`/`redis` service
    /// the user wants to skip in favor of a Temps-managed database with
    /// backup/restore support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_services: Vec<String>,
    /// Snapshot of the compose file's services (name/image/DB-hint), captured
    /// at project creation and refreshed after every successful deploy. Lets
    /// the settings-page exclusion checklist render without a live git fetch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compose_services: Vec<ComposeServiceSnapshot>,
    /// Legacy per-service opt-in for the minimal Linux capabilities (CHOWN,
    /// DAC_OVERRIDE, FOWNER, SETUID, SETGID) an entrypoint needs to fix
    /// ownership on a data directory and drop from root to a service user
    /// at container start. Every sandboxed Compose service now gets this
    /// set by default — the pattern is common to nearly all official
    /// images (databases like postgres/mysql/mariadb/mongo, but also web
    /// servers like nginx, and others such as Gitea), not a rare exception
    /// worth an opt-in. This field is kept only so older stored configs
    /// that listed services here keep deserializing; it no longer changes
    /// what gets granted (see `ComposeExecutor::RELAXED_CAPABILITIES`). An
    /// image that needs capabilities beyond that default set should use
    /// `unsandboxed_services` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relaxed_capability_services: Vec<String>,
    /// Compose services for which Temps must not inject its default runtime
    /// sandbox (`cap_drop: ALL`, `no-new-privileges`, and the PID limit).
    /// This is an explicit compatibility escape hatch for images whose own
    /// startup/runtime model cannot operate inside that sandbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsandboxed_services: Vec<String>,
}

/// The specific well-known service family a compose service's image matches,
/// when it matches one Temps can deploy as a managed `external_services` row
/// instead. Drives the "deploy this as a Temps-managed service" recommendation
/// in `GitSettings.tsx` and the deploy-log message — kept separate from
/// `ComposeServiceSnapshot::looks_like_database` (which stays a plain bool)
/// because that field only gates the unrelated "may need elevated Linux
/// capabilities" warning, and S3/MinIO images need this classification
/// without tripping that warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComposeServiceFamily {
    Postgres,
    Mariadb,
    Mongodb,
    Redis,
    S3,
}

/// One service parsed from a compose file, as persisted onto
/// [`DockerComposeConfig::compose_services`]. A smaller shape than
/// `temps_presets::ComposeServicePreview` (drops `depends_on` and environment
/// keys, which this settings surface does not render) since `temps-entities`
/// cannot depend on `temps-presets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposeServiceSnapshot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub looks_like_database: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_service_type: Option<ComposeServiceFamily>,
    /// Port mappings declared by this service after combining the repository
    /// Compose file with the user override. `target` is the container port the
    /// proxy must use; `published` is only Docker's optional host-side port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ComposePortMapping>,
    /// HTTP path discovered from this service's loopback Compose healthcheck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
}

/// A Docker Compose service port mapping, reduced to the information the UI
/// needs to build a public route without confusing host and container ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComposePortMapping {
    /// Port inside the service container. Temps routes traffic to this port.
    pub target: u16,
    /// Optional port published on the Docker host by Compose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<u16>,
    /// Transport protocol. Compose defaults to TCP.
    #[serde(default = "default_compose_port_protocol")]
    pub protocol: String,
}

fn default_compose_port_protocol() -> String {
    "tcp".to_string()
}

/// A port that should be exposed publicly through the proxy for a compose service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposePublicPort {
    /// Compose service name (e.g. "web", "clickhouse")
    pub service: String,
    /// Container port to expose (e.g. 8123)
    pub port: u16,
    /// Optional port published on the Docker host by Compose. The proxy uses
    /// this port when Temps runs on the host or reaches a remote node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<u16>,
    /// Optional user override for the public health probe. When absent, Temps
    /// uses the path discovered from this service's Compose healthcheck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
}

/// Hostname label of every public Compose route, in `public_ports` order.
///
/// A service's first public port keeps the plain service name, so existing
/// `{service}--{env}` URLs are unchanged. Each additional port on the same
/// service is labelled `{service}-{port}`, giving every public port of a
/// service its own stable hostname.
pub fn compose_public_route_labels(public_ports: &[ComposePublicPort]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    public_ports
        .iter()
        .map(|route| {
            if seen.insert(route.service.as_str()) {
                route.service.clone()
            } else {
                format!("{}-{}", route.service, route.port)
            }
        })
        .collect()
}

/// A Nixpacks build provider.
///
/// `Auto` serializes as the native Nixpacks `...` marker, which includes the
/// provider detected from the project alongside any explicitly listed
/// providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum NixpacksProvider {
    #[serde(rename = "...", alias = "auto")]
    Auto,
    Node,
    Python,
    Rust,
    Go,
    Java,
    Php,
    Ruby,
    Deno,
    Elixir,
    CSharp,
    FSharp,
    Dart,
    Swift,
    Zig,
    Scala,
    Haskell,
    Clojure,
    Crystal,
    Cobol,
    Gleam,
    Lunatic,
    Scheme,
    Static,
}

/// Nixpacks preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NixpacksConfig {
    /// Custom nixpacks.toml configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nixpacks_config: Option<String>,

    /// Ordered providers used to create the Nixpacks plan.
    ///
    /// An empty list delegates provider selection to the repository config or
    /// Nixpacks auto-detection. Include `...` to combine auto-detection with
    /// explicitly selected providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<NixpacksProvider>,
}

/// Static site preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct StaticConfig {
    /// Directory containing static files (default: ".")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_dir: Option<String>,
}

/// Node.js preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodeJsConfig {
    /// Node version (default: "20")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_version: Option<String>,

    /// Custom install command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Custom build command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Custom start command (default: "npm start")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,
}

/// Union type for all preset configurations
/// This allows storing any preset config in the database while maintaining type safety
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromJsonQueryResult)]
#[serde(tag = "preset", rename_all = "lowercase")]
pub enum PresetConfig {
    #[serde(rename = "nextjs")]
    NextJs(NextJsConfig),
    Vite(ViteConfig),
    Astro(AstroConfig),
    Nuxt(NuxtConfig),
    Remix(RemixConfig),
    #[serde(rename = "sveltekit")]
    SvelteKit(SvelteKitConfig),
    #[serde(rename = "solidstart")]
    SolidStart(SolidStartConfig),
    Angular(AngularConfig),
    Vue(VueConfig),
    React(ReactConfig),
    Docusaurus(DocusaurusConfig),
    Rsbuild(RsbuildConfig),
    Python(PythonConfig),
    #[serde(rename = "fastapi")]
    FastApi(FastApiConfig),
    Flask(FlaskConfig),
    Django(DjangoConfig),
    Rails(RailsConfig),
    Go(GoConfig),
    Rust(RustConfig),
    Java(JavaConfig),
    Laravel(LaravelConfig),
    Dockerfile(DockerfileConfig),
    #[serde(rename = "docker-compose")]
    DockerCompose(DockerComposeConfig),
    Nixpacks(NixpacksConfig),
    Static(StaticConfig),
    #[serde(rename = "nodejs")]
    NodeJs(NodeJsConfig),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_public_route_labels_keep_first_port_plain_and_suffix_the_rest() {
        let route = |service: &str, port: u16| ComposePublicPort {
            service: service.to_string(),
            port,
            ..Default::default()
        };
        let labels = compose_public_route_labels(&[
            route("trawl", 3000),
            route("api", 8080),
            route("trawl", 9222),
            route("trawl", 4000),
        ]);
        assert_eq!(labels, vec!["trawl", "api", "trawl-9222", "trawl-4000"]);
    }

    #[test]
    fn test_preset_serialization() {
        let preset = Preset::NextJs;
        let json = serde_json::to_string(&preset).unwrap();
        assert_eq!(json, "\"nextjs\"");

        let compose = Preset::DockerCompose;
        let json = serde_json::to_string(&compose).unwrap();
        println!("DockerCompose serializes to: {}", json);
        assert_eq!(json, "\"docker-compose\"");
    }

    #[test]
    fn test_preset_deserialization() {
        let json = "\"nextjs\"";
        let preset: Preset = serde_json::from_str(json).unwrap();
        assert_eq!(preset, Preset::NextJs);
    }

    #[test]
    fn test_preset_from_str() {
        use std::str::FromStr;

        assert_eq!(Preset::from_str("nextjs").unwrap(), Preset::NextJs);
        assert_eq!(Preset::from_str("NextJs").unwrap(), Preset::NextJs);
        assert_eq!(Preset::from_str("nodejs").unwrap(), Preset::NodeJs);
        assert_eq!(Preset::from_str("node").unwrap(), Preset::NodeJs);
        // Catalog variants are not database enum values.
        assert!(Preset::from_str("nixpacks-node").is_err());
        assert!(Preset::from_str("nixpacks-python").is_err());
        assert!(Preset::from_str("invalid").is_err());
    }

    #[test]
    fn test_nixpacks_provider_serialization_and_validation() {
        assert_eq!(
            serde_json::to_string(&NixpacksProvider::Auto).unwrap(),
            "\"...\""
        );
        assert_eq!(
            serde_json::from_str::<NixpacksProvider>("\"auto\"").unwrap(),
            NixpacksProvider::Auto
        );
        assert_eq!(
            serde_json::from_str::<NixpacksProvider>("\"node\"").unwrap(),
            NixpacksProvider::Node
        );
        assert_eq!(
            serde_json::to_string(&NixpacksProvider::CSharp).unwrap(),
            "\"csharp\""
        );
        assert_eq!(
            serde_json::to_string(&NixpacksProvider::Static).unwrap(),
            "\"static\""
        );
        assert!(serde_json::from_str::<NixpacksProvider>("\"not-real\"").is_err());
    }

    #[test]
    fn test_dockerfile_variant_is_backward_compatible_and_typed() {
        let legacy: DockerfileConfig =
            serde_json::from_value(serde_json::json!({ "dockerfilePath": "Dockerfile" })).unwrap();
        assert_eq!(legacy.variant, DockerfileVariant::File);

        let custom = DockerfileConfig {
            variant: DockerfileVariant::Custom,
            ..Default::default()
        };
        let json = serde_json::to_value(custom).unwrap();
        assert_eq!(json["variant"], "custom");
    }

    #[test]
    fn dockerfile_config_round_trips_image_template_runtime() {
        let config = DockerfileConfig {
            image_runtime: Some(ImageRuntimeConfig {
                image_ref: "registry.example.test/keycloak:27.0.0".to_string(),
                command: Some(vec!["start".to_string(), "--optimized".to_string()]),
                health_check_path: Some("/realms/master".to_string()),
            }),
            ..Default::default()
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json["imageRuntime"]["imageRef"],
            "registry.example.test/keycloak:27.0.0"
        );
        assert_eq!(
            serde_json::from_value::<DockerfileConfig>(json).unwrap(),
            config
        );
    }

    #[test]
    fn test_nixpacks_config_supports_ordered_multiple_providers() {
        let config = NixpacksConfig {
            nixpacks_config: None,
            providers: vec![NixpacksProvider::Auto, NixpacksProvider::Python],
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["providers"], serde_json::json!(["...", "python"]));
        assert_eq!(
            serde_json::from_value::<NixpacksConfig>(json).unwrap(),
            config
        );
    }

    #[test]
    fn test_legacy_nixpacks_config_defaults_to_auto_detection() {
        let config: NixpacksConfig = serde_json::from_value(serde_json::json!({
            "nixpacksConfig": "[start]\ncmd = \"npm start\""
        }))
        .unwrap();
        assert!(config.providers.is_empty());
        assert_eq!(
            config.nixpacks_config.as_deref(),
            Some("[start]\ncmd = \"npm start\"")
        );
    }

    #[test]
    fn test_preset_display() {
        assert_eq!(Preset::NextJs.to_string(), "nextjs");
    }

    #[test]
    fn test_compose_service_snapshot_camel_case_round_trip() {
        let snapshot = ComposeServiceSnapshot {
            name: "postgres".to_string(),
            image: Some("postgres:17-alpine".to_string()),
            looks_like_database: true,
            detected_service_type: None,
            ports: vec![ComposePortMapping {
                target: 5432,
                published: Some(15432),
                protocol: "tcp".to_string(),
            }],
            health_check_path: None,
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "postgres",
                "image": "postgres:17-alpine",
                "looksLikeDatabase": true,
                "ports": [{"target": 5432, "published": 15432, "protocol": "tcp"}]
            })
        );
        let round_tripped: ComposeServiceSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, snapshot);
    }

    #[test]
    fn test_compose_service_snapshot_omits_missing_image() {
        let snapshot = ComposeServiceSnapshot {
            name: "hub".to_string(),
            image: None,
            looks_like_database: false,
            detected_service_type: None,
            ..Default::default()
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "name": "hub", "looksLikeDatabase": false })
        );
    }

    #[test]
    fn test_compose_service_snapshot_persists_detected_service_type() {
        let snapshot = ComposeServiceSnapshot {
            name: "db".to_string(),
            image: Some("postgres:17-alpine".to_string()),
            looks_like_database: true,
            detected_service_type: Some(ComposeServiceFamily::Postgres),
            ..Default::default()
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "db",
                "image": "postgres:17-alpine",
                "looksLikeDatabase": true,
                "detectedServiceType": "postgres"
            })
        );
        let round_tripped: ComposeServiceSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, snapshot);
    }

    #[test]
    fn test_compose_service_snapshot_omits_missing_detected_service_type() {
        let snapshot = ComposeServiceSnapshot {
            name: "minio".to_string(),
            image: Some("minio/minio:latest".to_string()),
            looks_like_database: false,
            detected_service_type: Some(ComposeServiceFamily::S3),
            ..Default::default()
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "minio",
                "image": "minio/minio:latest",
                "looksLikeDatabase": false,
                "detectedServiceType": "s3"
            })
        );
    }

    #[test]
    fn test_docker_compose_config_omits_empty_relaxed_capability_services() {
        let cfg = DockerComposeConfig {
            compose_path: Some("compose.yml".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_value(&cfg).unwrap();
        assert!(json.get("relaxedCapabilityServices").is_none());
        assert!(json.get("unsandboxedServices").is_none());
    }
}
