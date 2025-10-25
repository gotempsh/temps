//! Language and Framework detection
//!
//! This module provides a two-level detection system:
//! - **Language**: Base runtime (Node.js, Python, Rust, etc.) - what nixpacks detects
//! - **Framework**: Specific framework within that language (Astro, Django, Axum, etc.)
//!
//! This separation allows:
//! - Better categorization in UI
//! - Language-specific optimizations + framework-specific overrides
//! - Support for custom Dockerfiles while tracking the underlying language

use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::debug;

/// Programming language/runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[serde(rename = "nodejs")]
    NodeJs,
    Python,
    Rust,
    Go,
    Java,
    Scala,
    Php,
    Ruby,
    Swift,
    Zig,
    Deno,
    Elixir,
    #[serde(rename = "csharp")]
    CSharp,
    Dart,
    Static,
    Unknown,
}

impl Language {
    pub fn name(&self) -> &'static str {
        match self {
            Self::NodeJs => "Node.js",
            Self::Python => "Python",
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::Scala => "Scala",
            Self::Php => "PHP",
            Self::Ruby => "Ruby",
            Self::Swift => "Swift",
            Self::Zig => "Zig",
            Self::Deno => "Deno",
            Self::Elixir => "Elixir",
            Self::CSharp => "C#",
            Self::Dart => "Dart",
            Self::Static => "Static Files",
            Self::Unknown => "Unknown",
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Self::NodeJs => "nodejs",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Scala => "scala",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Swift => "swift",
            Self::Zig => "zig",
            Self::Deno => "deno",
            Self::Elixir => "elixir",
            Self::CSharp => "csharp",
            Self::Dart => "dart",
            Self::Static => "static",
            Self::Unknown => "unknown",
        }
    }

    /// Detect language from project files
    pub fn detect(path: &Path) -> Self {
        if path.join("package.json").exists() {
            return Self::NodeJs;
        }
        if path.join("requirements.txt").exists()
            || path.join("pyproject.toml").exists()
            || path.join("Pipfile").exists()
        {
            return Self::Python;
        }
        if path.join("Cargo.toml").exists() {
            return Self::Rust;
        }
        if path.join("go.mod").exists() {
            return Self::Go;
        }
        // Check for Scala first (build.sbt is Scala-specific)
        if path.join("build.sbt").exists() {
            return Self::Scala;
        }
        // Java detection (Maven or Gradle without build.sbt)
        if path.join("pom.xml").exists() || path.join("build.gradle").exists() {
            return Self::Java;
        }
        if path.join("composer.json").exists() {
            return Self::Php;
        }
        if path.join("Gemfile").exists() {
            return Self::Ruby;
        }
        if path.join("deno.json").exists() || path.join("deno.jsonc").exists() {
            return Self::Deno;
        }
        if path.join("mix.exs").exists() {
            return Self::Elixir;
        }
        if path.join("pubspec.yaml").exists() {
            return Self::Dart;
        }
        // Check for C# project files
        if std::fs::read_dir(path)
            .ok()
            .and_then(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|e| {
                        e.path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .map(|ext| ext == "csproj")
                            .unwrap_or(false)
                    })
                    .then_some(())
            })
            .is_some()
        {
            return Self::CSharp;
        }

        Self::Unknown
    }
}

/// Framework - can be language-specific or generic
///
/// This enum represents the framework/build tool used for a project.
/// Some frameworks are language-specific (Node.js frameworks, Python frameworks),
/// while others are generic options that work with any language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    /// Node.js frameworks - each requires different build/start commands
    NodeJs(NodeFramework),
    /// Python frameworks - each has different WSGI/ASGI servers
    Python(PythonFramework),
    /// Java build tools - Maven vs Gradle require different commands
    Java(JavaBuildTool),
    /// Scala build tools - SBT vs Maven vs Gradle
    Scala(ScalaBuildTool),
    /// Nixpacks - let nixpacks auto-detect and build
    Nixpacks,
    /// Custom Dockerfile - user provides their own Dockerfile
    Dockerfile,
    /// No specific framework - use language defaults
    None,
}

/// Node.js frameworks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeFramework {
    Astro,
    #[serde(rename = "nextjs")]
    NextJs,
    #[serde(rename = "nestjs")]
    NestJs,
    Nuxt,
    Remix,
    Vite,
    Vue,
    Express,
    #[serde(rename = "react")]
    ReactApp,
    Svelte,
    Solid,
    #[serde(rename = "generic")]
    Generic,
}

/// Python frameworks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PythonFramework {
    Django,
    Flask,
    #[serde(rename = "fastapi")]
    FastApi,
    Streamlit,
    #[serde(rename = "generic")]
    Generic,
}

/// Java build tools (each requires different build commands)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JavaBuildTool {
    Maven,
    Gradle,
    #[serde(rename = "generic")]
    Generic,
}

/// Scala build tools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScalaBuildTool {
    #[serde(rename = "sbt")]
    Sbt,
    Maven,
    Gradle,
    #[serde(rename = "generic")]
    Generic,
}


impl NodeFramework {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Astro => "Astro",
            Self::NextJs => "Next.js",
            Self::NestJs => "NestJS",
            Self::Nuxt => "Nuxt",
            Self::Remix => "Remix",
            Self::Vite => "Vite",
            Self::Vue => "Vue",
            Self::Express => "Express",
            Self::ReactApp => "React App",
            Self::Svelte => "Svelte",
            Self::Solid => "SolidJS",
            Self::Generic => "Generic Node.js",
        }
    }
}

impl PythonFramework {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Django => "Django",
            Self::Flask => "Flask",
            Self::FastApi => "FastAPI",
            Self::Streamlit => "Streamlit",
            Self::Generic => "Generic Python",
        }
    }
}

impl JavaBuildTool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Maven => "Maven",
            Self::Gradle => "Gradle",
            Self::Generic => "Generic Java",
        }
    }
}

impl ScalaBuildTool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sbt => "SBT",
            Self::Maven => "Maven",
            Self::Gradle => "Gradle",
            Self::Generic => "Generic Scala",
        }
    }
}


impl Framework {
    pub fn name(&self) -> String {
        match self {
            Self::NodeJs(fw) => fw.name().to_string(),
            Self::Python(fw) => fw.name().to_string(),
            Self::Java(tool) => tool.name().to_string(),
            Self::Scala(tool) => tool.name().to_string(),
            Self::Nixpacks => "Nixpacks".to_string(),
            Self::Dockerfile => "Dockerfile".to_string(),
            Self::None => "None".to_string(),
        }
    }

    pub fn slug(&self) -> String {
        match self {
            Self::NodeJs(NodeFramework::Astro) => "astro".to_string(),
            Self::NodeJs(NodeFramework::NextJs) => "nextjs".to_string(),
            Self::NodeJs(NodeFramework::NestJs) => "nestjs".to_string(),
            Self::NodeJs(NodeFramework::Nuxt) => "nuxt".to_string(),
            Self::NodeJs(NodeFramework::Remix) => "remix".to_string(),
            Self::NodeJs(NodeFramework::Vite) => "vite".to_string(),
            Self::NodeJs(NodeFramework::Vue) => "vue".to_string(),
            Self::NodeJs(NodeFramework::Express) => "express".to_string(),
            Self::NodeJs(NodeFramework::ReactApp) => "react".to_string(),
            Self::NodeJs(NodeFramework::Svelte) => "svelte".to_string(),
            Self::NodeJs(NodeFramework::Solid) => "solid".to_string(),
            Self::NodeJs(NodeFramework::Generic) => "nodejs_generic".to_string(),
            Self::Python(PythonFramework::Django) => "django".to_string(),
            Self::Python(PythonFramework::Flask) => "flask".to_string(),
            Self::Python(PythonFramework::FastApi) => "fastapi".to_string(),
            Self::Python(PythonFramework::Streamlit) => "streamlit".to_string(),
            Self::Python(PythonFramework::Generic) => "python_generic".to_string(),
            Self::Java(JavaBuildTool::Maven) => "maven".to_string(),
            Self::Java(JavaBuildTool::Gradle) => "gradle".to_string(),
            Self::Java(JavaBuildTool::Generic) => "java_generic".to_string(),
            Self::Scala(ScalaBuildTool::Sbt) => "sbt".to_string(),
            Self::Scala(ScalaBuildTool::Maven) => "scala_maven".to_string(),
            Self::Scala(ScalaBuildTool::Gradle) => "scala_gradle".to_string(),
            Self::Scala(ScalaBuildTool::Generic) => "scala_generic".to_string(),
            Self::Nixpacks => "nixpacks".to_string(),
            Self::Dockerfile => "dockerfile".to_string(),
            Self::None => "none".to_string(),
        }
    }

    /// Check if this framework requires language-specific detection
    pub fn is_language_specific(&self) -> bool {
        matches!(
            self,
            Self::NodeJs(_) | Self::Python(_) | Self::Java(_) | Self::Scala(_)
        )
    }

    /// Check if this framework is generic (works with any language)
    pub fn is_generic(&self) -> bool {
        matches!(self, Self::Nixpacks | Self::Dockerfile | Self::None)
    }
}

/// Combined language and framework detection result
#[derive(Debug, Clone)]
pub struct DetectionResult {
    pub language: Language,
    pub framework: Framework,
}

/// Detect both language and framework from a project directory
pub fn detect_language_and_framework(path: &Path) -> DetectionResult {
    let language = Language::detect(path);

    let framework = match language {
        Language::NodeJs => detect_nodejs_framework(path),
        Language::Python => detect_python_framework(path),
        Language::Java => detect_java_build_tool(path),
        Language::Scala => detect_scala_build_tool(path),
        // For other languages, no specific framework detection needed
        // They will use language defaults (Framework::None means use language's default build/deploy)
        _ => Framework::None,
    };

    debug!(
        "Detected language: {}, framework: {}",
        language.name(),
        framework.name()
    );

    DetectionResult {
        language,
        framework,
    }
}

/// Detect Node.js framework from package.json
fn detect_nodejs_framework(path: &Path) -> Framework {
    let package_json_path = path.join("package.json");
    if !package_json_path.exists() {
        return Framework::NodeJs(NodeFramework::Generic);
    }

    let content = match std::fs::read_to_string(&package_json_path) {
        Ok(c) => c,
        Err(_) => return Framework::NodeJs(NodeFramework::Generic),
    };

    let pkg: serde_json::Value = match serde_json::from_str(&content) {
        Ok(p) => p,
        Err(_) => return Framework::NodeJs(NodeFramework::Generic),
    };

    let has_dep = |name: &str| {
        pkg.get("dependencies")
            .and_then(|d| d.get(name))
            .is_some()
            || pkg
                .get("devDependencies")
                .and_then(|d| d.get(name))
                .is_some()
    };

    // Priority order matters
    if has_dep("astro") {
        Framework::NodeJs(NodeFramework::Astro)
    } else if has_dep("next") {
        Framework::NodeJs(NodeFramework::NextJs)
    } else if has_dep("@nestjs/core") {
        Framework::NodeJs(NodeFramework::NestJs)
    } else if has_dep("nuxt") {
        Framework::NodeJs(NodeFramework::Nuxt)
    } else if has_dep("@remix-run/react") {
        Framework::NodeJs(NodeFramework::Remix)
    } else if has_dep("svelte") {
        Framework::NodeJs(NodeFramework::Svelte)
    } else if has_dep("solid-js") {
        Framework::NodeJs(NodeFramework::Solid)
    } else if has_dep("vite") {
        Framework::NodeJs(NodeFramework::Vite)
    } else if has_dep("vue") {
        Framework::NodeJs(NodeFramework::Vue)
    } else if has_dep("express") {
        Framework::NodeJs(NodeFramework::Express)
    } else if has_dep("react") && has_dep("react-scripts") {
        Framework::NodeJs(NodeFramework::ReactApp)
    } else {
        Framework::NodeJs(NodeFramework::Generic)
    }
}

/// Detect Python framework from requirements.txt or imports
fn detect_python_framework(path: &Path) -> Framework {
    // Check requirements.txt
    if let Ok(content) = std::fs::read_to_string(path.join("requirements.txt")) {
        let lower = content.to_lowercase();
        if lower.contains("django") {
            return Framework::Python(PythonFramework::Django);
        }
        if lower.contains("fastapi") {
            return Framework::Python(PythonFramework::FastApi);
        }
        if lower.contains("flask") {
            return Framework::Python(PythonFramework::Flask);
        }
        if lower.contains("streamlit") {
            return Framework::Python(PythonFramework::Streamlit);
        }
    }

    // Check pyproject.toml
    if let Ok(content) = std::fs::read_to_string(path.join("pyproject.toml")) {
        let lower = content.to_lowercase();
        if lower.contains("django") {
            return Framework::Python(PythonFramework::Django);
        }
        if lower.contains("fastapi") {
            return Framework::Python(PythonFramework::FastApi);
        }
        if lower.contains("flask") {
            return Framework::Python(PythonFramework::Flask);
        }
        if lower.contains("streamlit") {
            return Framework::Python(PythonFramework::Streamlit);
        }
    }

    Framework::Python(PythonFramework::Generic)
}

/// Detect Java build tool from project files
fn detect_java_build_tool(path: &Path) -> Framework {
    // Check for Maven (pom.xml)
    if path.join("pom.xml").exists() {
        return Framework::Java(JavaBuildTool::Maven);
    }

    // Check for Gradle (build.gradle or build.gradle.kts)
    if path.join("build.gradle").exists() || path.join("build.gradle.kts").exists() {
        return Framework::Java(JavaBuildTool::Gradle);
    }

    Framework::Java(JavaBuildTool::Generic)
}

/// Detect Scala build tool from project files
fn detect_scala_build_tool(path: &Path) -> Framework {
    // Check for SBT (build.sbt)
    if path.join("build.sbt").exists() {
        return Framework::Scala(ScalaBuildTool::Sbt);
    }

    // Check for Maven (pom.xml with Scala)
    if path.join("pom.xml").exists() {
        if let Ok(content) = std::fs::read_to_string(path.join("pom.xml")) {
            if content.contains("scala") {
                return Framework::Scala(ScalaBuildTool::Maven);
            }
        }
    }

    // Check for Gradle (build.gradle or build.gradle.kts with Scala)
    if path.join("build.gradle").exists() || path.join("build.gradle.kts").exists() {
        return Framework::Scala(ScalaBuildTool::Gradle);
    }

    Framework::Scala(ScalaBuildTool::Generic)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_language_detection() {
        let temp_dir = TempDir::new().unwrap();

        // Node.js
        fs::write(temp_dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(Language::detect(temp_dir.path()), Language::NodeJs);

        // Python
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("requirements.txt"), "").unwrap();
        assert_eq!(Language::detect(temp_dir.path()), Language::Python);

        // Rust
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(Language::detect(temp_dir.path()), Language::Rust);
    }

    #[test]
    fn test_framework_helpers() {
        // Test language-specific frameworks
        assert!(Framework::NodeJs(NodeFramework::Astro).is_language_specific());
        assert!(Framework::Python(PythonFramework::Django).is_language_specific());
        assert!(Framework::Java(JavaBuildTool::Maven).is_language_specific());

        // Test generic frameworks
        assert!(Framework::Nixpacks.is_generic());
        assert!(Framework::Dockerfile.is_generic());
        assert!(Framework::None.is_generic());

        assert!(!Framework::NodeJs(NodeFramework::Astro).is_generic());
        assert!(!Framework::Nixpacks.is_language_specific());
    }

    #[test]
    fn test_combined_detection() {
        let temp_dir = TempDir::new().unwrap();
        let pkg = r#"{"dependencies": {"astro": "^4.0.0"}}"#;
        fs::write(temp_dir.path().join("package.json"), pkg).unwrap();

        let result = detect_language_and_framework(temp_dir.path());
        assert_eq!(result.language, Language::NodeJs);
        assert_eq!(result.framework, Framework::NodeJs(NodeFramework::Astro));
    }

    #[test]
    fn test_language_without_framework_detection() {
        // Test Rust - should return Framework::None (use language defaults)
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[dependencies]\naxum = \"0.7\"",
        )
        .unwrap();

        let result = detect_language_and_framework(temp_dir.path());
        assert_eq!(result.language, Language::Rust);
        assert_eq!(result.framework, Framework::None);

        // Test Go - should return Framework::None (use language defaults)
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("go.mod"),
            "module example.com/myapp\n\nrequire github.com/gin-gonic/gin v1.9.0",
        )
        .unwrap();

        let result = detect_language_and_framework(temp_dir.path());
        assert_eq!(result.language, Language::Go);
        assert_eq!(result.framework, Framework::None);
    }

    #[test]
    fn test_json_serialization() {
        // Test Node.js framework serialization
        let framework = Framework::NodeJs(NodeFramework::Astro);
        let json = serde_json::to_string(&framework).unwrap();
        assert_eq!(json, r#"{"nodejs":"astro"}"#);

        // Test Java build tool serialization
        let framework = Framework::Java(JavaBuildTool::Maven);
        let json = serde_json::to_string(&framework).unwrap();
        assert_eq!(json, r#"{"java":"maven"}"#);

        // Test generic frameworks
        let framework = Framework::Nixpacks;
        let json = serde_json::to_string(&framework).unwrap();
        assert_eq!(json, r#""nixpacks""#);

        let framework = Framework::Dockerfile;
        let json = serde_json::to_string(&framework).unwrap();
        assert_eq!(json, r#""dockerfile""#);

        let framework = Framework::None;
        let json = serde_json::to_string(&framework).unwrap();
        assert_eq!(json, r#""none""#);

        // Test deserialization
        let json = r#"{"nodejs":"nextjs"}"#;
        let framework: Framework = serde_json::from_str(json).unwrap();
        assert_eq!(framework, Framework::NodeJs(NodeFramework::NextJs));

        let json = r#""nixpacks""#;
        let framework: Framework = serde_json::from_str(json).unwrap();
        assert_eq!(framework, Framework::Nixpacks);
    }
}
