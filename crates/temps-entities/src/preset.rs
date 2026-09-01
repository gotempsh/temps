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

    /// Get the human-readable display name
    pub fn display_name(&self) -> &'static str {
        match self {
            Preset::NextJs => "Next.js",
            Preset::Vite => "Vite",
            Preset::Astro => "Astro",
            Preset::Nuxt => "Nuxt",
            Preset::Remix => "Remix",
            Preset::SvelteKit => "SvelteKit",
            Preset::SolidStart => "SolidStart",
            Preset::Angular => "Angular",
            Preset::Vue => "Vue",
            Preset::React => "React",
            Preset::Docusaurus => "Docusaurus",
            Preset::Rsbuild => "Rsbuild",
            Preset::Python => "Python",
            Preset::FastApi => "FastAPI",
            Preset::Flask => "Flask",
            Preset::Django => "Django",
            Preset::Rails => "Ruby on Rails",
            Preset::Go => "Go",
            Preset::Rust => "Rust",
            Preset::Java => "Java",
            Preset::Laravel => "Laravel",
            Preset::Dockerfile => "Dockerfile",
            Preset::DockerCompose => "Docker Compose",
            Preset::Nixpacks => "Nixpacks",
            Preset::Autopack => "Autopack",
            Preset::Static => "Static Site",
            Preset::NodeJs => "Node.js",
        }
    }

    /// Get the language/runtime for this preset
    pub fn language(&self) -> &'static str {
        match self {
            Preset::NextJs
            | Preset::Vite
            | Preset::Astro
            | Preset::Nuxt
            | Preset::Remix
            | Preset::SvelteKit
            | Preset::SolidStart
            | Preset::Angular
            | Preset::Vue
            | Preset::React
            | Preset::Docusaurus
            | Preset::Rsbuild
            | Preset::NodeJs => "node",
            Preset::Python | Preset::FastApi | Preset::Flask | Preset::Django => "python",
            Preset::Rails => "ruby",
            Preset::Go => "go",
            Preset::Rust => "rust",
            Preset::Java => "java",
            Preset::Laravel => "php",
            Preset::Dockerfile
            | Preset::DockerCompose
            | Preset::Nixpacks
            | Preset::Autopack
            | Preset::Static => "generic",
        }
    }

    /// Check if this preset supports static site generation
    pub fn is_static_capable(&self) -> bool {
        matches!(
            self,
            Preset::NextJs
                | Preset::Vite
                | Preset::Astro
                | Preset::Nuxt
                | Preset::SvelteKit
                | Preset::Angular
                | Preset::Vue
                | Preset::React
                | Preset::Docusaurus
                | Preset::Rsbuild
                | Preset::Static
        )
    }

    /// Check if this preset requires a runtime server
    pub fn requires_server(&self) -> bool {
        !matches!(self, Preset::Static)
    }

    /// Get the default exposed port for this preset
    ///
    /// Returns the typical port that this framework/runtime listens on.
    /// Returns None for static presets that don't have a runtime server.
    pub fn exposed_port(&self) -> Option<u16> {
        match self {
            // Node.js frameworks - most use 3000 by default
            Preset::NextJs => Some(3000),
            Preset::Vite => Some(5173),  // Vite dev server default
            Preset::Astro => Some(4321), // Astro dev server default
            Preset::Nuxt => Some(3000),
            Preset::Remix => Some(3000),
            Preset::SvelteKit => Some(5173),
            Preset::SolidStart => Some(3000),
            Preset::Angular => Some(4200), // Angular CLI default
            Preset::Vue => Some(8080),     // Vue CLI default
            Preset::React => Some(3000),   // Create React App default
            Preset::Docusaurus => Some(3000),
            Preset::Rsbuild => Some(3000),
            Preset::NodeJs => Some(3000), // Generic Node.js

            // Python frameworks
            Preset::Python => Some(8000),  // Generic Python web apps
            Preset::FastApi => Some(8000), // FastAPI/uvicorn default
            Preset::Flask => Some(5000),   // Flask default
            Preset::Django => Some(8000),  // Django default

            // Ruby frameworks
            Preset::Rails => Some(3000), // Rails default

            // Go
            Preset::Go => Some(8080), // Common Go web server port

            // Rust
            Preset::Rust => Some(8080), // Common Rust web server port

            // Java
            Preset::Java => Some(8080), // Common Java web server port (Spring Boot, etc.)

            // PHP frameworks
            Preset::Laravel => Some(8000), // Laravel artisan serve default

            // Generic/static presets - no default port
            Preset::Dockerfile => None,    // User-defined
            Preset::DockerCompose => None, // Multiple services, user-configured
            Preset::Nixpacks => None,      // Auto-detected
            Preset::Autopack => None,      // Auto-detected
            Preset::Static => None,        // No server
        }
    }

    /// Get the icon URL for this preset (logo or framework icon)
    pub fn icon_url(&self) -> Option<&'static str> {
        match self {
            // Node.js frameworks
            Preset::NextJs => Some("https://cdn.simpleicons.org/nextdotjs/000000"),
            Preset::Vite => Some("https://cdn.simpleicons.org/vite/646CFF"),
            Preset::Astro => Some("https://cdn.simpleicons.org/astro/FF5D01"),
            Preset::Nuxt => Some("https://cdn.simpleicons.org/nuxtdotjs/00DC82"),
            Preset::Remix => Some("https://cdn.simpleicons.org/remix/000000"),
            Preset::SvelteKit => Some("https://cdn.simpleicons.org/svelte/FF3E00"),
            Preset::SolidStart => Some("https://cdn.simpleicons.org/solid/2C4F7C"),
            Preset::Angular => Some("https://cdn.simpleicons.org/angular/DD0031"),
            Preset::Vue => Some("https://cdn.simpleicons.org/vuedotjs/4FC08D"),
            Preset::React => Some("https://cdn.simpleicons.org/react/61DAFB"),
            Preset::Docusaurus => Some("https://cdn.simpleicons.org/docusaurus/3ECC5F"),
            Preset::Rsbuild => Some("https://cdn.simpleicons.org/rsbuild/FFC700"),
            Preset::NodeJs => Some("https://cdn.simpleicons.org/nodedotjs/339933"),

            // Python frameworks
            Preset::Python => Some("https://cdn.simpleicons.org/python/3776AB"),
            Preset::FastApi => Some("https://cdn.simpleicons.org/fastapi/009688"),
            Preset::Flask => Some("https://cdn.simpleicons.org/flask/000000"),
            Preset::Django => Some("https://cdn.simpleicons.org/django/092E20"),

            // Ruby frameworks
            Preset::Rails => Some("https://cdn.simpleicons.org/rubyonrails/CC0000"),

            // Go
            Preset::Go => Some("https://cdn.simpleicons.org/go/00ADD8"),

            // Rust
            Preset::Rust => Some("https://cdn.simpleicons.org/rust/000000"),

            // Java
            Preset::Java => Some("https://cdn.simpleicons.org/openjdk/437291"),

            // PHP frameworks
            Preset::Laravel => Some("https://cdn.simpleicons.org/laravel/FF2D20"),

            // Generic presets
            Preset::Dockerfile => Some("https://cdn.simpleicons.org/docker/2496ED"),
            Preset::DockerCompose => Some("https://cdn.simpleicons.org/docker/2496ED"),
            Preset::Nixpacks => None, // No specific icon
            Preset::Autopack => None, // No specific icon
            Preset::Static => Some("https://cdn.simpleicons.org/html5/E34F26"),
        }
    }

    /// Get the project type category for this preset
    pub fn project_type(&self) -> &'static str {
        match self {
            // Frontend frameworks
            Preset::NextJs | Preset::Nuxt | Preset::SvelteKit | Preset::SolidStart => "fullstack",
            Preset::Vite
            | Preset::Astro
            | Preset::Remix
            | Preset::Angular
            | Preset::Vue
            | Preset::React
            | Preset::Docusaurus
            | Preset::Rsbuild => "frontend",

            // Backend frameworks
            Preset::FastApi | Preset::Flask | Preset::Django | Preset::Rails | Preset::Laravel => {
                "backend"
            }

            // Runtime/language presets
            Preset::Python | Preset::Go | Preset::Rust | Preset::Java | Preset::NodeJs => "runtime",

            // Generic presets
            Preset::Dockerfile | Preset::DockerCompose | Preset::Nixpacks | Preset::Autopack => {
                "container"
            }
            Preset::Static => "static",
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
    /// Compose service names granted the minimal Linux capabilities
    /// (CHOWN, DAC_OVERRIDE, FOWNER, SETUID, SETGID) their entrypoint needs
    /// to fix ownership on a data directory and drop from root to a service
    /// user at container start — a pattern common to many official images,
    /// not just databases (postgres/mysql/mariadb/mongo, but also e.g.
    /// Gitea). Off by default: Temps drops all capabilities from every
    /// compose service for defense in depth, and only grants this back for
    /// a service the user has explicitly opted in.
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

impl NixpacksProvider {
    /// Provider identifier expected by Nixpacks build plans.
    pub fn nixpacks_name(self) -> &'static str {
        match self {
            Self::Auto => "...",
            Self::Node => "node",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Deno => "deno",
            Self::Elixir => "elixir",
            Self::CSharp => "c#",
            Self::FSharp => "f#",
            Self::Dart => "dart",
            Self::Swift => "swift",
            Self::Zig => "zig",
            Self::Scala => "scala",
            Self::Haskell => "haskell",
            Self::Clojure => "clojure",
            Self::Crystal => "crystal",
            Self::Cobol => "cobol",
            Self::Gleam => "gleam",
            Self::Lunatic => "lunatic",
            Self::Scheme => "scheme",
            Self::Static => "staticfile",
        }
    }

    pub fn variant_slug(self) -> &'static str {
        match self {
            Self::Auto => "nixpacks",
            Self::Node => "nixpacks-node",
            Self::Python => "nixpacks-python",
            Self::Rust => "nixpacks-rust",
            Self::Go => "nixpacks-go",
            Self::Java => "nixpacks-java",
            Self::Php => "nixpacks-php",
            Self::Ruby => "nixpacks-ruby",
            Self::Deno => "nixpacks-deno",
            Self::Elixir => "nixpacks-elixir",
            Self::CSharp => "nixpacks-csharp",
            Self::FSharp => "nixpacks-fsharp",
            Self::Dart => "nixpacks-dart",
            Self::Swift => "nixpacks-swift",
            Self::Zig => "nixpacks-zig",
            Self::Scala => "nixpacks-scala",
            Self::Haskell => "nixpacks-haskell",
            Self::Clojure => "nixpacks-clojure",
            Self::Crystal => "nixpacks-crystal",
            Self::Cobol => "nixpacks-cobol",
            Self::Gleam => "nixpacks-gleam",
            Self::Lunatic => "nixpacks-lunatic",
            Self::Scheme => "nixpacks-scheme",
            Self::Static => "nixpacks-static",
        }
    }
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

impl PresetConfig {
    /// Create a default configuration for a given preset
    pub fn default_for_preset(preset: Preset) -> Self {
        match preset {
            Preset::NextJs => PresetConfig::NextJs(NextJsConfig::default()),
            Preset::Vite => PresetConfig::Vite(ViteConfig::default()),
            Preset::Astro => PresetConfig::Astro(AstroConfig::default()),
            Preset::Nuxt => PresetConfig::Nuxt(NuxtConfig::default()),
            Preset::Remix => PresetConfig::Remix(RemixConfig::default()),
            Preset::SvelteKit => PresetConfig::SvelteKit(SvelteKitConfig::default()),
            Preset::SolidStart => PresetConfig::SolidStart(SolidStartConfig::default()),
            Preset::Angular => PresetConfig::Angular(AngularConfig::default()),
            Preset::Vue => PresetConfig::Vue(VueConfig::default()),
            Preset::React => PresetConfig::React(ReactConfig::default()),
            Preset::Docusaurus => PresetConfig::Docusaurus(DocusaurusConfig::default()),
            Preset::Rsbuild => PresetConfig::Rsbuild(RsbuildConfig::default()),
            Preset::Python => PresetConfig::Python(PythonConfig::default()),
            Preset::FastApi => PresetConfig::FastApi(FastApiConfig::default()),
            Preset::Flask => PresetConfig::Flask(FlaskConfig::default()),
            Preset::Django => PresetConfig::Django(DjangoConfig::default()),
            Preset::Rails => PresetConfig::Rails(RailsConfig::default()),
            Preset::Go => PresetConfig::Go(GoConfig::default()),
            Preset::Rust => PresetConfig::Rust(RustConfig::default()),
            Preset::Java => PresetConfig::Java(JavaConfig::default()),
            Preset::Laravel => PresetConfig::Laravel(LaravelConfig::default()),
            Preset::Dockerfile => PresetConfig::Dockerfile(DockerfileConfig::default()),
            Preset::DockerCompose => PresetConfig::DockerCompose(DockerComposeConfig::default()),
            Preset::Nixpacks => PresetConfig::Nixpacks(NixpacksConfig::default()),
            Preset::Autopack => PresetConfig::Nixpacks(NixpacksConfig::default()),
            Preset::Static => PresetConfig::Static(StaticConfig::default()),
            Preset::NodeJs => PresetConfig::NodeJs(NodeJsConfig::default()),
        }
    }

    /// Parse a JSON value into a PresetConfig for the given preset.
    ///
    /// The JSON value should contain the config fields for the preset
    /// (e.g., `{"dockerfilePath": "docker/Dockerfile"}` for Dockerfile preset).
    /// The `preset` tag is added automatically based on the provided preset enum.
    pub fn parse_for_preset(preset: &Preset, value: &serde_json::Value) -> Result<Self, String> {
        // Build a tagged JSON object by injecting the "preset" discriminator
        let preset_tag = preset.as_str();
        let tagged = match value {
            serde_json::Value::Object(map) => {
                let mut tagged_map = map.clone();
                tagged_map.insert(
                    "preset".to_string(),
                    serde_json::Value::String(preset_tag.to_string()),
                );
                serde_json::Value::Object(tagged_map)
            }
            _ => {
                return Err(format!(
                    "Expected JSON object for preset config, got: {}",
                    value
                ));
            }
        };

        serde_json::from_value(tagged)
            .map_err(|e| format!("Failed to parse preset config for '{}': {}", preset_tag, e))
    }

    /// Get the preset type from this configuration
    pub fn preset_type(&self) -> Preset {
        match self {
            PresetConfig::NextJs(_) => Preset::NextJs,
            PresetConfig::Vite(_) => Preset::Vite,
            PresetConfig::Astro(_) => Preset::Astro,
            PresetConfig::Nuxt(_) => Preset::Nuxt,
            PresetConfig::Remix(_) => Preset::Remix,
            PresetConfig::SvelteKit(_) => Preset::SvelteKit,
            PresetConfig::SolidStart(_) => Preset::SolidStart,
            PresetConfig::Angular(_) => Preset::Angular,
            PresetConfig::Vue(_) => Preset::Vue,
            PresetConfig::React(_) => Preset::React,
            PresetConfig::Docusaurus(_) => Preset::Docusaurus,
            PresetConfig::Rsbuild(_) => Preset::Rsbuild,
            PresetConfig::Python(_) => Preset::Python,
            PresetConfig::FastApi(_) => Preset::FastApi,
            PresetConfig::Flask(_) => Preset::Flask,
            PresetConfig::Django(_) => Preset::Django,
            PresetConfig::Rails(_) => Preset::Rails,
            PresetConfig::Go(_) => Preset::Go,
            PresetConfig::Rust(_) => Preset::Rust,
            PresetConfig::Java(_) => Preset::Java,
            PresetConfig::Laravel(_) => Preset::Laravel,
            PresetConfig::Dockerfile(_) => Preset::Dockerfile,
            PresetConfig::DockerCompose(_) => Preset::DockerCompose,
            PresetConfig::Nixpacks(_) => Preset::Nixpacks,
            PresetConfig::Static(_) => Preset::Static,
            PresetConfig::NodeJs(_) => Preset::NodeJs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(NixpacksProvider::CSharp.nixpacks_name(), "c#");
        assert_eq!(
            serde_json::to_string(&NixpacksProvider::Static).unwrap(),
            "\"static\""
        );
        assert_eq!(NixpacksProvider::Static.nixpacks_name(), "staticfile");
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
    fn test_parse_for_preset_nixpacks_with_providers() {
        let value = serde_json::json!({
            "providers": ["node", "python"],
            "nixpacksConfig": "[start]\ncmd = \"npm start\""
        });
        let config = PresetConfig::parse_for_preset(&Preset::Nixpacks, &value).unwrap();
        match config {
            PresetConfig::Nixpacks(cfg) => {
                assert_eq!(
                    cfg.providers,
                    vec![NixpacksProvider::Node, NixpacksProvider::Python]
                );
                assert_eq!(
                    cfg.nixpacks_config.as_deref(),
                    Some("[start]\ncmd = \"npm start\"")
                );
            }
            other => panic!("expected Nixpacks config, got {other:?}"),
        }
    }

    #[test]
    fn test_preset_language() {
        assert_eq!(Preset::NextJs.language(), "node");
        assert_eq!(Preset::FastApi.language(), "python");
        assert_eq!(Preset::Rails.language(), "ruby");
        assert_eq!(Preset::Dockerfile.language(), "generic");
    }

    #[test]
    fn test_preset_display() {
        assert_eq!(Preset::NextJs.to_string(), "nextjs");
        assert_eq!(Preset::NextJs.display_name(), "Next.js");
    }

    #[test]
    fn test_parse_for_preset_dockerfile() {
        let value =
            serde_json::json!({"dockerfilePath": "docker/Dockerfile", "buildContext": "./api"});
        let config = PresetConfig::parse_for_preset(&Preset::Dockerfile, &value).unwrap();
        match config {
            PresetConfig::Dockerfile(cfg) => {
                assert_eq!(cfg.dockerfile_path, Some("docker/Dockerfile".to_string()));
                assert_eq!(cfg.build_context, Some("./api".to_string()));
            }
            _ => panic!("Expected Dockerfile config"),
        }
    }

    #[test]
    fn test_parse_for_preset_dockerfile_empty() {
        let value = serde_json::json!({});
        let config = PresetConfig::parse_for_preset(&Preset::Dockerfile, &value).unwrap();
        match config {
            PresetConfig::Dockerfile(cfg) => {
                assert_eq!(cfg.dockerfile_path, None);
                assert_eq!(cfg.build_context, None);
            }
            _ => panic!("Expected Dockerfile config"),
        }
    }

    #[test]
    fn test_parse_for_preset_rejects_non_object() {
        let value = serde_json::json!("not an object");
        let result = PresetConfig::parse_for_preset(&Preset::Dockerfile, &value);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected JSON object"));
    }

    #[test]
    fn test_parse_for_preset_nextjs() {
        let value = serde_json::json!({"buildCommand": "next build"});
        let config = PresetConfig::parse_for_preset(&Preset::NextJs, &value).unwrap();
        match config {
            PresetConfig::NextJs(cfg) => {
                assert_eq!(cfg.build_command, Some("next build".to_string()));
            }
            _ => panic!("Expected NextJs config"),
        }
    }

    #[test]
    fn test_parse_for_preset_docker_compose_with_override() {
        let value = serde_json::json!({
            "preset": "docker-compose",
            "composePath": "docker-compose.yaml",
            "composeOverride": "services:\n  plausible:\n    ports:\n      - 48080:80",
            "publicPorts": [{"service": "plausible", "port": 80}]
        });
        let config = PresetConfig::parse_for_preset(&Preset::DockerCompose, &value).unwrap();
        match config {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.compose_path, Some("docker-compose.yaml".to_string()));
                assert_eq!(
                    cfg.compose_override,
                    Some("services:\n  plausible:\n    ports:\n      - 48080:80".to_string())
                );
                assert_eq!(cfg.public_ports.len(), 1);
                assert_eq!(cfg.public_ports[0].service, "plausible");
                assert_eq!(cfg.public_ports[0].port, 80);
            }
            _ => panic!("Expected DockerCompose config"),
        }
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
    fn test_docker_compose_config_persists_compose_services() {
        let value = serde_json::json!({
            "preset": "docker-compose",
            "composePath": "compose.yml",
            "excludedServices": ["postgres"],
            "composeServices": [
                {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
                {"name": "hub", "image": "ghcr.io/getpaseo/hub:latest", "looksLikeDatabase": false}
            ]
        });
        let config = PresetConfig::parse_for_preset(&Preset::DockerCompose, &value).unwrap();
        match config {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
                assert_eq!(cfg.compose_services.len(), 2);
                assert_eq!(cfg.compose_services[0].name, "postgres");
                assert!(cfg.compose_services[0].looks_like_database);
                assert!(!cfg.compose_services[1].looks_like_database);
            }
            _ => panic!("Expected DockerCompose config"),
        }
    }

    #[test]
    fn test_docker_compose_config_persists_relaxed_capability_services() {
        let value = serde_json::json!({
            "preset": "docker-compose",
            "composePath": "compose.yml",
            "relaxedCapabilityServices": ["db"]
        });
        let config = PresetConfig::parse_for_preset(&Preset::DockerCompose, &value).unwrap();
        match config {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.relaxed_capability_services, vec!["db".to_string()]);
            }
            _ => panic!("Expected DockerCompose config"),
        }
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

    #[test]
    fn test_docker_compose_config_persists_unsandboxed_services() {
        let value = serde_json::json!({
            "preset": "docker-compose",
            "composePath": "compose.yml",
            "unsandboxedServices": ["webserver"]
        });
        let config = PresetConfig::parse_for_preset(&Preset::DockerCompose, &value).unwrap();
        match config {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.unsandboxed_services, vec!["webserver".to_string()]);
            }
            _ => panic!("Expected DockerCompose config"),
        }
    }
}
