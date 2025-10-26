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

    #[sea_orm(string_value = "static")]
    Static,

    // Node.js runtime (for custom node apps)
    #[serde(rename = "nodejs")]
    #[sea_orm(string_value = "nodejs")]
    NodeJs,
}

impl Preset {
    /// Get the preset name as a string
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
            Preset::Nixpacks => "nixpacks",
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
            Preset::Nixpacks => "Nixpacks",
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
            Preset::Dockerfile | Preset::Nixpacks | Preset::Static => "generic",
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
            Preset::Dockerfile => None, // User-defined
            Preset::Nixpacks => None,   // Auto-detected
            Preset::Static => None,     // No server
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
            Preset::Nixpacks => None, // No specific icon
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
            Preset::Dockerfile | Preset::Nixpacks => "container",
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
            "nixpacks" => Ok(Preset::Nixpacks),
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

/// Dockerfile preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DockerfileConfig {
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

/// Nixpacks preset configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NixpacksConfig {
    /// Custom nixpacks.toml configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nixpacks_config: Option<String>,
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
            Preset::Nixpacks => PresetConfig::Nixpacks(NixpacksConfig::default()),
            Preset::Static => PresetConfig::Static(StaticConfig::default()),
            Preset::NodeJs => PresetConfig::NodeJs(NodeJsConfig::default()),
        }
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
        assert!(Preset::from_str("invalid").is_err());
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
}
