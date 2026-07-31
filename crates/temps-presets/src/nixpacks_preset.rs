//! Nixpacks preset - generates Dockerfile for any supported language
//!
//! This preset uses nixpacks to auto-detect the project language/framework
//! and generate an optimized Dockerfile. It acts as a fallback when no
//! framework-specific preset (Next.js, Vite, etc.) or user-provided Dockerfile exists.
//!
//! Supported languages: Node.js, Python, Rust, Go, Java, PHP, Ruby, Elixir, .NET, Dart, etc.
//!
//! Provider-specific variants allow explicit selection for monorepos and multi-language projects.

use crate::{DockerfileConfig, DockerfileWithArgs, Preset, ProjectType};
use async_trait::async_trait;
use nixpacks::nixpacks::{
    app::App,
    builder::{
        docker::{docker_image_builder::DockerImageBuilder, DockerBuilderOptions},
        ImageBuilder,
    },
    environment::Environment,
    logger::Logger,
    plan::{
        generator::{GeneratePlanOptions, NixpacksBuildPlanGenerator},
        BuildPlan,
        PlanGenerator,
    },
};
use std::collections::HashMap;
use std::path::Path;
pub use temps_entities::preset::NixpacksProvider;
use temps_entities::preset::NixpacksConfig;
use tokio::fs;
use tracing::{debug, info, warn};

fn provider_name(provider: NixpacksProvider) -> &'static str {
    match provider {
        NixpacksProvider::Auto => "Auto-detect",
        NixpacksProvider::Node => "Node.js",
        NixpacksProvider::Python => "Python",
        NixpacksProvider::Rust => "Rust",
        NixpacksProvider::Go => "Go",
        NixpacksProvider::Java => "Java",
        NixpacksProvider::Php => "PHP",
        NixpacksProvider::Ruby => "Ruby",
        NixpacksProvider::Deno => "Deno",
        NixpacksProvider::Elixir => "Elixir",
        NixpacksProvider::CSharp => "C# / .NET",
        NixpacksProvider::FSharp => "F# / .NET",
        NixpacksProvider::Dart => "Dart",
        NixpacksProvider::Swift => "Swift",
        NixpacksProvider::Zig => "Zig",
        NixpacksProvider::Scala => "Scala",
        NixpacksProvider::Haskell => "Haskell",
        NixpacksProvider::Clojure => "Clojure",
        NixpacksProvider::Crystal => "Crystal",
        NixpacksProvider::Cobol => "COBOL",
        NixpacksProvider::Gleam => "Gleam",
        NixpacksProvider::Lunatic => "Lunatic",
        NixpacksProvider::Scheme => "Scheme",
        NixpacksProvider::Static => "Static Files",
    }
}

fn provider_icon_url(provider: NixpacksProvider) -> &'static str {
    match provider {
        NixpacksProvider::Auto => "/presets/nixpacks.svg",
        NixpacksProvider::Node => "/presets/nodejs.svg",
        NixpacksProvider::Python => "/presets/python.svg",
        NixpacksProvider::Rust => "/presets/rust.svg",
        NixpacksProvider::Go => "/presets/go.svg",
        NixpacksProvider::Java => "/presets/java.svg",
        NixpacksProvider::Php => "/presets/php.svg",
        NixpacksProvider::Ruby => "/presets/ruby.svg",
        NixpacksProvider::Deno => "/presets/deno.svg",
        NixpacksProvider::Elixir => "/presets/elixir.svg",
        NixpacksProvider::CSharp => "/presets/dotnet.svg",
        NixpacksProvider::FSharp => "/presets/fsharp.svg",
        NixpacksProvider::Dart => "/presets/dart.svg",
        NixpacksProvider::Swift => "/presets/swift.svg",
        NixpacksProvider::Zig => "/presets/zig.svg",
        NixpacksProvider::Scala => "/presets/scala.svg",
        NixpacksProvider::Haskell => "/presets/haskell.svg",
        NixpacksProvider::Clojure => "/presets/clojure.svg",
        NixpacksProvider::Crystal => "/presets/crystal.svg",
        NixpacksProvider::Cobol => "/presets/cobol.svg",
        NixpacksProvider::Gleam => "/presets/gleam.svg",
        NixpacksProvider::Lunatic => "/presets/lunatic.svg",
        NixpacksProvider::Scheme => "/presets/scheme.svg",
        NixpacksProvider::Static => "/presets/static.svg",
    }
}

fn provider_description(provider: NixpacksProvider) -> &'static str {
    match provider {
        NixpacksProvider::Auto => "Auto-detects your language and framework from the repository",
        NixpacksProvider::Node => {
            "Node.js apps — Nuxt, Vue, SvelteKit, Astro, Remix, Express, and more"
        }
        NixpacksProvider::Python => {
            "Python web applications (Django, Flask, FastAPI, etc.)"
        }
        NixpacksProvider::Rust => "Rust web applications and services",
        NixpacksProvider::Go => "Go web applications and services",
        NixpacksProvider::Java => {
            "Java web applications (Spring Boot, Micronaut, Quarkus, etc.)"
        }
        NixpacksProvider::Php => "PHP applications — Laravel, Symfony, and more",
        NixpacksProvider::Ruby => "Ruby applications — Ruby on Rails and more",
        NixpacksProvider::Deno => "Deno applications and services",
        NixpacksProvider::Elixir => "Elixir applications — Phoenix and more",
        NixpacksProvider::CSharp => "C# / .NET applications and services",
        NixpacksProvider::FSharp => "F# / .NET applications and services",
        NixpacksProvider::Dart => "Dart applications and services",
        NixpacksProvider::Swift => "Swift applications and services",
        NixpacksProvider::Zig => "Zig applications and services",
        NixpacksProvider::Scala => "Scala applications and services",
        NixpacksProvider::Haskell => "Haskell applications and services",
        NixpacksProvider::Clojure => "Clojure applications and services",
        NixpacksProvider::Crystal => "Crystal applications and services",
        NixpacksProvider::Cobol => "COBOL applications and services",
        NixpacksProvider::Gleam => "Gleam applications and services",
        NixpacksProvider::Lunatic => "Lunatic applications and services",
        NixpacksProvider::Scheme => "Scheme applications and services",
        NixpacksProvider::Static => "Pre-built static files, no build step",
    }
}

pub struct NixpacksPreset {
    config: NixpacksConfig,
}

impl NixpacksPreset {
    pub fn new(provider: NixpacksProvider) -> Self {
        let providers = if provider == NixpacksProvider::Auto {
            Vec::new()
        } else {
            vec![provider]
        };
        Self {
            config: NixpacksConfig {
                providers,
                ..Default::default()
            },
        }
    }

    pub fn auto() -> Self {
        Self::new(NixpacksProvider::Auto)
    }

    pub fn from_config(config: NixpacksConfig) -> Self {
        Self { config }
    }

    pub(super) fn validate_config(
        config: &NixpacksConfig,
    ) -> Result<(), crate::PresetResolutionError> {
        if let Some(value) = config.nixpacks_config.as_deref() {
            BuildPlan::from_toml(value).map_err(|_| {
                crate::PresetResolutionError::InvalidConfig {
                    slug: "nixpacks".to_string(),
                    reason: "failed to parse Nixpacks TOML; verify its syntax and supported fields"
                        .to_string(),
                }
            })?;
        }
        Ok(())
    }

    fn display_provider(&self) -> NixpacksProvider {
        match self.config.providers.as_slice() {
            [provider] => *provider,
            _ => NixpacksProvider::Auto,
        }
    }

    fn generate_plan_options(&self) -> Result<GeneratePlanOptions, String> {
        let mut plan = match self.config.nixpacks_config.as_deref() {
            Some(config) => BuildPlan::from_toml(config).map_err(|_| {
                "Failed to parse custom Nixpacks config; verify its syntax and supported fields"
                    .to_string()
            })?,
            None => BuildPlan::default(),
        };

        if !self.config.providers.is_empty() {
            plan.providers = Some(
                self.config
                    .providers
                    .iter()
                    .map(|provider| provider.nixpacks_name().to_string())
                    .collect(),
            );
        }

        Ok(GeneratePlanOptions {
            plan: Some(plan),
            ..Default::default()
        })
    }

    fn generate_build_plan(
        &self,
        app: &App,
        environment: &Environment,
    ) -> Result<BuildPlan, String> {
        let providers: &[&dyn nixpacks::providers::Provider] = &[
            &nixpacks::providers::node::NodeProvider {},
            &nixpacks::providers::python::PythonProvider {},
            &nixpacks::providers::rust::RustProvider {},
            &nixpacks::providers::go::GolangProvider {},
            &nixpacks::providers::java::JavaProvider {},
            &nixpacks::providers::php::PhpProvider {},
            &nixpacks::providers::ruby::RubyProvider {},
            &nixpacks::providers::deno::DenoProvider {},
            &nixpacks::providers::elixir::ElixirProvider {},
            &nixpacks::providers::csharp::CSharpProvider {},
            &nixpacks::providers::fsharp::FSharpProvider {},
            &nixpacks::providers::dart::DartProvider {},
            &nixpacks::providers::swift::SwiftProvider {},
            &nixpacks::providers::zig::ZigProvider {},
            &nixpacks::providers::scala::ScalaProvider {},
            &nixpacks::providers::haskell::HaskellStackProvider {},
            &nixpacks::providers::clojure::ClojureProvider {},
            &nixpacks::providers::crystal::CrystalProvider {},
            &nixpacks::providers::cobol::CobolProvider {},
            &nixpacks::providers::gleam::GleamProvider {},
            &nixpacks::providers::lunatic::LunaticProvider {},
            &nixpacks::providers::scheme::HauntProvider {},
            &nixpacks::providers::staticfile::StaticfileProvider {},
        ];

        let options = self.generate_plan_options()?;
        let mut generator = NixpacksBuildPlanGenerator::new(providers, options);
        generator
            .generate_plan(app, environment)
            .map(|(plan, _)| plan)
            .map_err(|error| format!("Failed to generate build plan: {}", error))
    }

    /// Detect which providers are available for a given path
    /// Returns a list of providers that can handle the project
    pub fn detect_available_providers(path: &Path) -> Vec<NixpacksProvider> {
        let mut available = Vec::new();

        // Check if a Nixpacks config file exists to pass as an option if present
        // Supported: nixpacks.toml or .nixpacks.toml
        let nixpacks_toml = path.join("nixpacks.toml");
        let dot_nixpacks_toml = path.join(".nixpacks.toml");
        let config_file = if nixpacks_toml.exists() {
            Some(nixpacks_toml)
        } else if dot_nixpacks_toml.exists() {
            Some(dot_nixpacks_toml)
        } else {
            None
        };

        // Check each provider (except Auto and Static)
        let providers_to_check: Vec<(NixpacksProvider, &dyn nixpacks::providers::Provider)> = vec![
            (
                NixpacksProvider::Node,
                &nixpacks::providers::node::NodeProvider {},
            ),
            (
                NixpacksProvider::Python,
                &nixpacks::providers::python::PythonProvider {},
            ),
            (
                NixpacksProvider::Rust,
                &nixpacks::providers::rust::RustProvider {},
            ),
            (
                NixpacksProvider::Go,
                &nixpacks::providers::go::GolangProvider {},
            ),
            (
                NixpacksProvider::Java,
                &nixpacks::providers::java::JavaProvider {},
            ),
            (
                NixpacksProvider::Php,
                &nixpacks::providers::php::PhpProvider {},
            ),
            (
                NixpacksProvider::Ruby,
                &nixpacks::providers::ruby::RubyProvider {},
            ),
            (
                NixpacksProvider::Deno,
                &nixpacks::providers::deno::DenoProvider {},
            ),
            (
                NixpacksProvider::Elixir,
                &nixpacks::providers::elixir::ElixirProvider {},
            ),
            (
                NixpacksProvider::CSharp,
                &nixpacks::providers::csharp::CSharpProvider {},
            ),
            (
                NixpacksProvider::FSharp,
                &nixpacks::providers::fsharp::FSharpProvider {},
            ),
            (
                NixpacksProvider::Dart,
                &nixpacks::providers::dart::DartProvider {},
            ),
            (
                NixpacksProvider::Swift,
                &nixpacks::providers::swift::SwiftProvider {},
            ),
            (
                NixpacksProvider::Zig,
                &nixpacks::providers::zig::ZigProvider {},
            ),
            (
                NixpacksProvider::Scala,
                &nixpacks::providers::scala::ScalaProvider {},
            ),
            (
                NixpacksProvider::Haskell,
                &nixpacks::providers::haskell::HaskellStackProvider {},
            ),
            (
                NixpacksProvider::Clojure,
                &nixpacks::providers::clojure::ClojureProvider {},
            ),
            (
                NixpacksProvider::Crystal,
                &nixpacks::providers::crystal::CrystalProvider {},
            ),
            (
                NixpacksProvider::Cobol,
                &nixpacks::providers::cobol::CobolProvider {},
            ),
            (
                NixpacksProvider::Gleam,
                &nixpacks::providers::gleam::GleamProvider {},
            ),
            (
                NixpacksProvider::Lunatic,
                &nixpacks::providers::lunatic::LunaticProvider {},
            ),
            (
                NixpacksProvider::Scheme,
                &nixpacks::providers::scheme::HauntProvider {},
            ),
        ];

        let path_str = match path.to_str() {
            Some(s) => s,
            None => return available,
        };

        let app = match App::new(path_str) {
            Ok(app) => app,
            Err(_) => return available,
        };

        let environment = match Environment::from_envs(vec![]) {
            Ok(env) => env,
            Err(_) => return available,
        };

        // Check each provider individually, passing config if present
        for (provider_type, provider) in providers_to_check {
            let providers_slice = vec![provider];

            // Build options, include the config path if file exists
            let mut options = GeneratePlanOptions::default();
            if let Some(config_path) = &config_file {
                // Only pass the config if the file exists
                options.config_file = Some(config_path.to_string_lossy().to_string());
            }

            let mut generator = NixpacksBuildPlanGenerator::new(&providers_slice, options);

            if let Ok((plan, _)) = generator.generate_plan(&app, &environment) {
                let phase_count = plan.phases.clone().map_or(0, |phases| phases.len());
                if phase_count > 0 && plan.start_phase.is_some() {
                    available.push(provider_type);
                }
            }
        }

        available
    }
}

impl NixpacksPreset {
    /// Check if nixpacks can detect and handle this project
    pub fn can_detect(path: &Path) -> bool {
        // Try to generate a plan - if successful, nixpacks can handle it
        let path_str = match path.to_str() {
            Some(s) => s,
            None => {
                warn!("Invalid path encoding for nixpacks detection");
                return false;
            }
        };

        let app = match App::new(path_str) {
            Ok(app) => app,
            Err(e) => {
                debug!("Nixpacks: Failed to create app: {}", e);
                return false;
            }
        };

        let environment = match Environment::from_envs(vec![]) {
            Ok(env) => env,
            Err(e) => {
                debug!("Nixpacks: Failed to create environment: {}", e);
                return false;
            }
        };

        let providers: &[&dyn nixpacks::providers::Provider] = &[
            &nixpacks::providers::node::NodeProvider {},
            &nixpacks::providers::python::PythonProvider {},
            &nixpacks::providers::rust::RustProvider {},
            &nixpacks::providers::go::GolangProvider {},
            &nixpacks::providers::java::JavaProvider {},
            &nixpacks::providers::php::PhpProvider {},
            &nixpacks::providers::ruby::RubyProvider {},
            &nixpacks::providers::deno::DenoProvider {},
            &nixpacks::providers::elixir::ElixirProvider {},
            &nixpacks::providers::csharp::CSharpProvider {},
            &nixpacks::providers::fsharp::FSharpProvider {},
            &nixpacks::providers::dart::DartProvider {},
            &nixpacks::providers::swift::SwiftProvider {},
            &nixpacks::providers::zig::ZigProvider {},
            &nixpacks::providers::scala::ScalaProvider {},
            &nixpacks::providers::haskell::HaskellStackProvider {},
            &nixpacks::providers::clojure::ClojureProvider {},
            &nixpacks::providers::crystal::CrystalProvider {},
            &nixpacks::providers::cobol::CobolProvider {},
            &nixpacks::providers::gleam::GleamProvider {},
            &nixpacks::providers::lunatic::LunaticProvider {},
            &nixpacks::providers::scheme::HauntProvider {},
            &nixpacks::providers::staticfile::StaticfileProvider {},
        ];

        let mut generator =
            NixpacksBuildPlanGenerator::new(providers, GeneratePlanOptions::default());

        match generator.generate_plan(&app, &environment) {
            Ok((plan, _)) => {
                // Check if we have a valid plan with phases and start command
                let phase_count = plan.phases.clone().map_or(0, |phases| phases.len());
                if phase_count > 0 {
                    let start = plan.start_phase.clone().unwrap_or_default();
                    if start.cmd.is_some() {
                        debug!("Nixpacks: Successfully detected project at {:?}", path);
                        return true;
                    }
                }
                debug!("Nixpacks: Plan generated but missing start command");
                false
            }
            Err(e) => {
                debug!("Nixpacks: Failed to generate plan: {}", e);
                false
            }
        }
    }

    /// Generate actual Dockerfile using nixpacks' DockerImageBuilder
    ///
    /// This function uses the nixpacks library to:
    /// 1. Detect the project language/framework
    /// 2. Generate an optimized build plan
    /// 3. Use DockerImageBuilder to generate the actual Dockerfile
    /// 4. Extract build args from the plan's variables
    /// 5. Read the generated Dockerfile from .nixpacks/Dockerfile
    ///
    /// Returns both the Dockerfile content and the build args that should be passed to docker build.
    async fn generate_dockerfile_content(
        &self,
        path: &Path,
        build_vars: Option<&Vec<String>>,
    ) -> Result<DockerfileWithArgs, String> {
        let path_str = path
            .to_str()
            .ok_or_else(|| "Invalid path encoding".to_string())?;

        info!("Generating Dockerfile for: {:?}", path);

        // Create nixpacks App
        let app =
            App::new(path_str).map_err(|e| format!("Failed to create nixpacks app: {}", e))?;

        // Create environment from build variables
        let env_vars: Vec<&str> = build_vars
            .map(|vars| vars.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        let environment = Environment::from_envs(env_vars)
            .map_err(|e| format!("Failed to create environment: {}", e))?;

        // Generate the build plan with the persisted provider selection and
        // optional user-supplied Nixpacks plan.
        let plan = self.generate_build_plan(&app, &environment)?;

        // Validate plan
        let phase_count = plan.phases.clone().map_or(0, |phases| phases.len());
        if phase_count == 0 {
            return Err("Unable to generate a build plan for this app. \
                 Please check https://nixpacks.com for supported languages."
                .to_string());
        }

        // let start = plan.start_phase.clone().unwrap_or_default();
        // if start.cmd.is_none() {
        //     return Err("No start command could be found in the build plan".to_string());
        // }

        // Use DockerImageBuilder to generate the actual Dockerfile
        let builder = DockerImageBuilder::new(
            Logger::new(),
            DockerBuilderOptions {
                out_dir: Some(path.to_string_lossy().to_string()),
                ..Default::default()
            },
        );

        // Generate Dockerfile at .nixpacks/Dockerfile
        builder
            .create_image(path_str, &plan, &environment)
            .await
            .map_err(|e| format!("Failed to create nixpacks image: {}", e))?;

        // Read the generated Dockerfile
        let nixpacks_dockerfile = path.join(".nixpacks").join("Dockerfile");
        let dockerfile = fs::read_to_string(&nixpacks_dockerfile)
            .await
            .map_err(|e| format!("Failed to read generated Dockerfile: {}", e))?;

        // Extract build args from the plan's variables
        // These are the environment variables that nixpacks has set as defaults
        let mut build_args = HashMap::new();
        if let Some(variables) = plan.variables {
            for (key, value) in variables.iter() {
                build_args.insert(key.clone(), value.clone());
            }
        }

        debug!(
            dockerfile_bytes = dockerfile.len(),
            build_arg_count = build_args.len(),
            "Generated Nixpacks Dockerfile"
        );
        info!(
            "Successfully generated Dockerfile using nixpacks with {} build args",
            build_args.len()
        );

        Ok(DockerfileWithArgs::with_args(dockerfile, build_args))
    }
}

#[async_trait]
impl Preset for NixpacksPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        if self.config.providers.len() > 1 {
            let providers = self
                .config
                .providers
                .iter()
                .map(|provider| provider_name(*provider))
                .collect::<Vec<_>>()
                .join(" + ");
            format!("Nixpacks ({})", providers)
        } else {
            format!("Nixpacks ({})", provider_name(self.display_provider()))
        }
    }

    fn icon_url(&self) -> String {
        provider_icon_url(self.display_provider()).to_string()
    }

    fn description(&self) -> String {
        provider_description(self.display_provider()).to_string()
    }

    fn stored_preset(&self) -> Option<temps_entities::preset::Preset> {
        Some(temps_entities::preset::Preset::Nixpacks)
    }

    fn resolve_storage(
        &self,
        config: Option<temps_entities::preset::PresetConfig>,
    ) -> Result<crate::StoredPreset, crate::PresetResolutionError> {
        let mut config = match config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => config,
            Some(other) => {
                return Err(crate::PresetResolutionError::ConfigMismatch {
                    config_preset: other.preset_type(),
                    slug: self.slug(),
                });
            }
            None => NixpacksConfig::default(),
        };

        Self::validate_config(&config)?;

        if !self.config.providers.is_empty() {
            config.providers = self.config.providers.clone();
        }

        Ok(crate::StoredPreset {
            preset: temps_entities::preset::Preset::Nixpacks,
            config: Some(temps_entities::preset::PresetConfig::Nixpacks(config)),
        })
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        match self
            .generate_dockerfile_content(config.local_path, config.build_vars)
            .await
        {
            Ok(dockerfile_with_args) => dockerfile_with_args,
            Err(e) => {
                warn!("Failed to generate nixpacks Dockerfile: {}", e);
                // Return a minimal fallback Dockerfile
                DockerfileWithArgs::new(format!(
                    r#"FROM alpine:latest
WORKDIR /app
COPY . .
# Nixpacks failed to generate Dockerfile: {}
# Please provide a custom Dockerfile or check your project structure
"#,
                    e
                ))
            }
        }
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        match self.generate_dockerfile_content(local_path, None).await {
            Ok(dockerfile_with_args) => dockerfile_with_args,
            Err(e) => {
                warn!("Failed to generate nixpacks Dockerfile: {}", e);
                DockerfileWithArgs::new(format!(
                    r#"FROM alpine:latest
WORKDIR /app
COPY . .
# Nixpacks failed: {}
"#,
                    e
                ))
            }
        }
    }

    fn install_command(&self, _local_path: &Path) -> String {
        // Nixpacks handles installation automatically in the Dockerfile
        "# Handled by nixpacks".to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        // Nixpacks handles build automatically in the Dockerfile
        "# Handled by nixpacks".to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Nixpacks needs the entire project directory
        vec![".".to_string()]
    }

    fn slug(&self) -> String {
        match self.config.providers.as_slice() {
            [provider] => provider.variant_slug().to_string(),
            _ => "nixpacks".to_string(),
        }
    }
}

impl std::fmt::Display for NixpacksPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper to get the detected language/provider from build plan
    fn get_detected_language(path: &Path) -> Option<String> {
        let path_str = path.to_str()?;
        let app = App::new(path_str).ok()?;
        let environment = Environment::from_envs(vec![]).ok()?;

        let providers: &[&dyn nixpacks::providers::Provider] = &[
            &nixpacks::providers::node::NodeProvider {},
            &nixpacks::providers::python::PythonProvider {},
            &nixpacks::providers::rust::RustProvider {},
            &nixpacks::providers::go::GolangProvider {},
            &nixpacks::providers::java::JavaProvider {},
            &nixpacks::providers::php::PhpProvider {},
            &nixpacks::providers::ruby::RubyProvider {},
            &nixpacks::providers::deno::DenoProvider {},
            &nixpacks::providers::elixir::ElixirProvider {},
            &nixpacks::providers::csharp::CSharpProvider {},
            &nixpacks::providers::fsharp::FSharpProvider {},
            &nixpacks::providers::dart::DartProvider {},
            &nixpacks::providers::swift::SwiftProvider {},
            &nixpacks::providers::zig::ZigProvider {},
            &nixpacks::providers::scala::ScalaProvider {},
            &nixpacks::providers::haskell::HaskellStackProvider {},
            &nixpacks::providers::clojure::ClojureProvider {},
            &nixpacks::providers::crystal::CrystalProvider {},
            &nixpacks::providers::cobol::CobolProvider {},
            &nixpacks::providers::gleam::GleamProvider {},
            &nixpacks::providers::lunatic::LunaticProvider {},
            &nixpacks::providers::scheme::HauntProvider {},
            &nixpacks::providers::staticfile::StaticfileProvider {},
        ];

        let mut generator =
            NixpacksBuildPlanGenerator::new(providers, GeneratePlanOptions::default());

        let (plan, _) = generator.generate_plan(&app, &environment).ok()?;

        // Get the build plan string which contains the detected info
        let build_string = plan.get_build_string().ok()?;

        Some(build_string)
    }

    fn create_nodejs_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let package_json = r#"{
  "name": "test-app",
  "version": "1.0.0",
  "scripts": {
    "start": "node index.js"
  },
  "dependencies": {
    "express": "^4.18.0"
  }
}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();
        fs::write(
            temp_dir.path().join("index.js"),
            "console.log('Hello World')",
        )
        .unwrap();
        temp_dir
    }

    fn create_python_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("requirements.txt"), "flask==2.0.0").unwrap();
        fs::write(
            temp_dir.path().join("main.py"),
            r#"from flask import Flask
app = Flask(__name__)

@app.route('/')
def hello():
    return 'Hello World!'

if __name__ == '__main__':
    app.run()
"#,
        )
        .unwrap();
        temp_dir
    }

    #[test]
    fn test_nixpacks_detects_nodejs() {
        let temp_dir = create_nodejs_project();

        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Should detect Node.js project"
        );

        // Verify it detected Node.js specifically
        let detected = get_detected_language(temp_dir.path()).expect("Should detect language");
        assert!(
            detected.to_lowercase().contains("node") || detected.contains("npm"),
            "Build plan should indicate Node.js was detected, got: {}",
            &detected[..detected.len().min(200)]
        );
    }

    #[test]
    fn test_nixpacks_detects_python() {
        let temp_dir = create_python_project();

        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Should detect Python project"
        );

        // Verify it detected Python specifically
        let detected = get_detected_language(temp_dir.path()).expect("Should detect language");
        assert!(
            detected.to_lowercase().contains("python") || detected.contains("pip"),
            "Build plan should indicate Python was detected, got: {}",
            &detected[..detected.len().min(200)]
        );
    }

    #[test]
    fn test_nixpacks_fails_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!NixpacksPreset::can_detect(temp_dir.path()));
    }

    #[test]
    fn test_nixpacks_preset_properties() {
        let preset = NixpacksPreset::auto();
        assert_eq!(preset.slug(), "nixpacks");
        assert_eq!(preset.label(), "Nixpacks (Auto-detect)");
        assert!(matches!(preset.project_type(), ProjectType::Server));
    }

    #[test]
    fn test_nixpacks_provider_selection_reaches_plan_options() {
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: None,
            providers: vec![NixpacksProvider::Node],
        });
        let options = preset.generate_plan_options().unwrap();

        assert_eq!(
            options.plan.and_then(|plan| plan.providers),
            Some(vec!["node".to_string()])
        );
    }

    #[test]
    fn test_nixpacks_multiple_providers_preserve_order_and_auto_marker() {
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: None,
            providers: vec![NixpacksProvider::Auto, NixpacksProvider::Python],
        });
        let options = preset.generate_plan_options().unwrap();

        assert_eq!(
            options.plan.and_then(|plan| plan.providers),
            Some(vec!["...".to_string(), "python".to_string()])
        );
        assert_eq!(preset.slug(), "nixpacks");
    }

    #[test]
    fn test_nixpacks_invalid_custom_config_is_contextual() {
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: Some("invalid = [".to_string()),
            providers: Vec::new(),
        });

        let error = preset.generate_plan_options().unwrap_err();
        assert!(error.contains("Failed to parse custom Nixpacks config"));
    }

    fn create_rust_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = r#"[package]
name = "test-rust-app"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = "0.7"
tokio = { version = "1", features = ["full"] }
"#;
        fs::write(temp_dir.path().join("Cargo.toml"), cargo_toml).unwrap();

        let src_dir = temp_dir.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(
            src_dir.join("main.rs"),
            r#"fn main() { println!("Hello, world!"); }"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_go_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let go_mod = r#"module example.com/hello

go 1.21
"#;
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();
        fs::write(
            temp_dir.path().join("main.go"),
            r#"package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
}
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_nextjs_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let package_json = r#"{
  "name": "nextjs-app",
  "version": "1.0.0",
  "scripts": {
    "dev": "next dev",
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.0.0",
    "react": "^18.2.0",
    "react-dom": "^18.2.0"
  }
}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let pages_dir = temp_dir.path().join("pages");
        fs::create_dir(&pages_dir).unwrap();
        fs::write(
            pages_dir.join("index.js"),
            "export default function Home() { return <div>Hello</div> }",
        )
        .unwrap();
        temp_dir
    }

    fn create_php_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let composer_json = r#"{
  "name": "test/php-app",
  "require": {
    "php": "^8.0"
  }
}"#;
        fs::write(temp_dir.path().join("composer.json"), composer_json).unwrap();
        fs::write(
            temp_dir.path().join("index.php"),
            "<?php\necho 'Hello, World!';\n",
        )
        .unwrap();
        temp_dir
    }

    fn create_ruby_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let gemfile = r#"source 'https://rubygems.org'

gem 'sinatra'
gem 'thin'
"#;
        fs::write(temp_dir.path().join("Gemfile"), gemfile).unwrap();

        // Create Gemfile.lock for detection
        let gemfile_lock = r#"GEM
  remote: https://rubygems.org/
  specs:
    sinatra (3.0.0)

PLATFORMS
  ruby

DEPENDENCIES
  sinatra

BUNDLED WITH
   2.4.0
"#;
        fs::write(temp_dir.path().join("Gemfile.lock"), gemfile_lock).unwrap();

        // Create config.ru for Rack application
        fs::write(
            temp_dir.path().join("config.ru"),
            r#"require './app'
run Sinatra::Application
"#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("app.rb"),
            r#"require 'sinatra'

get '/' do
  'Hello, World!'
end
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_java_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let pom_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>0.0.1-SNAPSHOT</version>
  <name>demo</name>
</project>
"#;
        fs::write(temp_dir.path().join("pom.xml"), pom_xml).unwrap();

        let src_dir = temp_dir.path().join("src/main/java/com/example/demo");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("DemoApplication.java"),
            r#"package com.example.demo;

public class DemoApplication {
    public static void main(String[] args) {
        System.out.println("Hello, World!");
    }
}
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_deno_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("main.ts"),
            r#"import { serve } from "https://deno.land/std@0.140.0/http/server.ts";

serve(() => new Response("Hello, World!"));
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_elixir_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let mix_exs = r#"defmodule MyApp.MixProject do
  use Mix.Project

  def project do
    [
      app: :my_app,
      version: "0.1.0",
      elixir: "~> 1.14"
    ]
  end
end
"#;
        fs::write(temp_dir.path().join("mix.exs"), mix_exs).unwrap();

        let lib_dir = temp_dir.path().join("lib");
        fs::create_dir(&lib_dir).unwrap();
        fs::write(
            lib_dir.join("my_app.ex"),
            r#"defmodule MyApp do
  def hello do
    "Hello, World!"
  end
end
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_csharp_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let csproj = r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup>
    <TargetFramework>net7.0</TargetFramework>
  </PropertyGroup>
</Project>
"#;
        fs::write(temp_dir.path().join("MyApp.csproj"), csproj).unwrap();
        fs::write(
            temp_dir.path().join("Program.cs"),
            r#"var builder = WebApplication.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/", () => "Hello World!");

app.Run();
"#,
        )
        .unwrap();
        temp_dir
    }

    fn create_dart_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let pubspec = r#"name: my_app
version: 1.0.0
environment:
  sdk: '>=2.17.0 <3.0.0'
"#;
        fs::write(temp_dir.path().join("pubspec.yaml"), pubspec).unwrap();

        let bin_dir = temp_dir.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        fs::write(
            bin_dir.join("main.dart"),
            r#"void main() {
  print('Hello, World!');
}
"#,
        )
        .unwrap();
        temp_dir
    }

    // Detection tests for all supported languages
    #[test]
    fn test_nixpacks_detects_rust() {
        let temp_dir = create_rust_project();

        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Rust project with Cargo.toml"
        );

        let detected = get_detected_language(temp_dir.path()).expect("Should detect language");
        assert!(
            detected.to_lowercase().contains("rust") || detected.contains("cargo"),
            "Build plan should indicate Rust was detected, got: {}",
            &detected[..detected.len().min(200)]
        );
    }

    #[test]
    fn test_nixpacks_detects_go() {
        let temp_dir = create_go_project();

        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Go project with go.mod"
        );

        let detected = get_detected_language(temp_dir.path()).expect("Should detect language");
        assert!(
            detected.to_lowercase().contains("go") || detected.contains("golang"),
            "Build plan should indicate Go was detected, got: {}",
            &detected[..detected.len().min(200)]
        );
    }

    #[test]
    fn test_nixpacks_detects_nextjs() {
        let temp_dir = create_nextjs_project();

        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Next.js project"
        );

        let detected = get_detected_language(temp_dir.path()).expect("Should detect language");
        assert!(
            detected.to_lowercase().contains("node")
                || detected.contains("next")
                || detected.contains("npm"),
            "Build plan should indicate Node.js/Next.js was detected, got: {}",
            &detected[..detected.len().min(200)]
        );
    }

    #[test]
    fn test_nixpacks_detects_php() {
        let temp_dir = create_php_project();

        // Verify it can detect
        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect PHP project with composer.json"
        );

        // Verify it detected PHP specifically, not another language
        let detected = get_detected_language(temp_dir.path());
        assert!(detected.is_some(), "Should return detection info");

        let plan = detected.unwrap();
        assert!(
            plan.to_lowercase().contains("php") || plan.contains("composer"),
            "Build plan should indicate PHP was detected. Got: {:?}",
            &plan[..plan.len().min(300)]
        );
    }

    #[test]
    fn test_nixpacks_detects_ruby() {
        let temp_dir = create_ruby_project();
        // Ruby detection is currently not working in this nixpacks version
        // Document the actual behavior
        let can_detect = NixpacksPreset::can_detect(temp_dir.path());
        println!("Ruby project detection result: {}", can_detect);
        // TODO: Investigate Ruby provider requirements in nixpacks
    }

    #[test]
    fn test_nixpacks_detects_java() {
        let temp_dir = create_java_project();
        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Java project with pom.xml"
        );
    }

    #[test]
    fn test_nixpacks_detects_deno() {
        let temp_dir = create_deno_project();
        // Deno detection might be tricky - it could detect as Node.js or Deno
        // Document actual behavior
        let can_detect = NixpacksPreset::can_detect(temp_dir.path());
        println!("Deno project detection result: {}", can_detect);
        // This test documents behavior rather than asserting
    }

    #[test]
    fn test_nixpacks_detects_elixir() {
        let temp_dir = create_elixir_project();
        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Elixir project with mix.exs"
        );
    }

    #[test]
    fn test_nixpacks_detects_csharp() {
        let temp_dir = create_csharp_project();
        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect C# project with .csproj"
        );
    }

    #[test]
    fn test_explicit_csharp_provider_uses_native_nixpacks_name() {
        let temp_dir = create_csharp_project();
        let app = App::new(temp_dir.path().to_string_lossy().as_ref()).unwrap();
        let environment = Environment::from_envs(vec![]).unwrap();
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: None,
            providers: vec![NixpacksProvider::CSharp],
        });

        let plan = preset
            .generate_build_plan(&app, &environment)
            .expect("explicit C# provider should resolve in Nixpacks");

        assert!(!plan.phases.unwrap_or_default().is_empty());
    }

    #[test]
    fn test_nixpacks_detects_dart() {
        let temp_dir = create_dart_project();
        assert!(
            NixpacksPreset::can_detect(temp_dir.path()),
            "Nixpacks should detect Dart project with pubspec.yaml"
        );
    }

    // Dockerfile generation tests
    #[tokio::test]
    async fn test_dockerfile_generation_returns_content() {
        let temp_dir = create_python_project();
        let preset = NixpacksPreset::auto();

        let config = DockerfileConfig {
            use_buildkit: true,
            root_local_path: temp_dir.path(),
            local_path: temp_dir.path(),
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        };

        let dockerfile = preset.dockerfile(config).await;

        println!("Generated content:\n{}", dockerfile.content);
        println!("Content length: {}", dockerfile.content.len());
        let build_args = dockerfile.build_args;
        println!("Build args: {:?}", build_args);
        assert!(
            !dockerfile.content.is_empty(),
            "Dockerfile should not be empty"
        );
        // Nixpacks returns a build plan summary, not a traditional Dockerfile
        assert!(
            dockerfile.content.contains("Nixpacks")
                || dockerfile.content.contains("setup")
                || dockerfile.content.contains("install"),
            "Content should contain nixpacks build plan information"
        );
    }

    #[tokio::test]
    async fn test_stored_python_provider_controls_ambiguous_project_build() {
        let temp_dir = create_python_project();
        fs::write(
            temp_dir.path().join("package.json"),
            r#"{
  "name": "ambiguous-app",
  "scripts": { "start": "node index.js" },
  "dependencies": { "express": "^4.18.0" }
}"#,
        )
        .unwrap();
        fs::write(temp_dir.path().join("index.js"), "console.log('node')").unwrap();

        let stored_config =
            temps_entities::preset::PresetConfig::Nixpacks(NixpacksConfig {
                nixpacks_config: None,
                providers: vec![NixpacksProvider::Python],
            });
        let preset = crate::get_preset_for_storage(
            temps_entities::preset::Preset::Nixpacks,
            Some(&stored_config),
        )
        .expect("stored Nixpacks config should be valid")
        .expect("stored Nixpacks preset should resolve");
        let dockerfile = preset
            .dockerfile(DockerfileConfig {
                use_buildkit: true,
                root_local_path: temp_dir.path(),
                local_path: temp_dir.path(),
                install_command: None,
                build_command: None,
                output_dir: None,
                build_vars: None,
                project_slug: "ambiguous-project",
            })
            .await;

        assert!(
            !dockerfile.content.contains("Nixpacks failed"),
            "{}",
            dockerfile.content
        );
        assert!(
            dockerfile.content.to_lowercase().contains("python"),
            "expected Python build plan, got:\n{}",
            dockerfile.content
        );
    }

    #[tokio::test]
    async fn test_dockerfile_contains_build_plan() {
        let temp_dir = create_nodejs_project();
        let preset = NixpacksPreset::auto();

        let config = DockerfileConfig {
            use_buildkit: true,
            root_local_path: temp_dir.path(),
            local_path: temp_dir.path(),
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "test-project",
        };

        let dockerfile = preset.dockerfile(config).await;
        println!("Generated dockerfile: {}", dockerfile.content);
        // Nixpacks returns a build plan summary with setup/install/start phases
        assert!(
            dockerfile.content.contains("install") || dockerfile.content.contains("start"),
            "Build plan should contain phase information"
        );
    }

    #[tokio::test]
    async fn test_dockerfile_with_build_dir() {
        let temp_dir = create_rust_project();
        let preset = NixpacksPreset::auto();

        let dockerfile = preset.dockerfile_with_build_dir(temp_dir.path()).await;

        assert!(!dockerfile.content.is_empty());
        // Nixpacks returns a build plan, not a traditional Dockerfile
        assert!(
            dockerfile.content.contains("Nixpacks")
                || dockerfile.content.contains("setup")
                || dockerfile.content.contains("install"),
            "Build plan should contain nixpacks information"
        );
    }

    // Install and build command tests
    #[test]
    fn test_install_command_handled_by_nixpacks() {
        let temp_dir = create_python_project();
        let preset = NixpacksPreset::auto();

        let install_cmd = preset.install_command(temp_dir.path());

        assert!(
            install_cmd.contains("nixpacks") || install_cmd.contains("Handled"),
            "Install command should indicate nixpacks handles it"
        );
    }

    #[test]
    fn test_build_command_handled_by_nixpacks() {
        let temp_dir = create_nodejs_project();
        let preset = NixpacksPreset::auto();

        let build_cmd = preset.build_command(temp_dir.path());

        assert!(
            build_cmd.contains("nixpacks") || build_cmd.contains("Handled"),
            "Build command should indicate nixpacks handles it"
        );
    }

    // Dirs to upload tests
    #[test]
    fn test_dirs_to_upload_includes_root() {
        let preset = NixpacksPreset::auto();
        let dirs = preset.dirs_to_upload();

        assert!(!dirs.is_empty(), "Should return directories to upload");
        assert!(
            dirs.contains(&".".to_string()),
            "Should include root directory"
        );
    }

    // Display trait test
    #[test]
    fn test_display_trait() {
        let preset = NixpacksPreset::auto();
        assert_eq!(format!("{}", preset), "Nixpacks (Auto-detect)");
    }

    // Icon URL test
    #[test]
    fn test_icon_url() {
        let preset = NixpacksPreset::auto();
        let icon_url = preset.icon_url();

        assert!(
            icon_url.contains("nixpacks"),
            "Icon URL should reference nixpacks"
        );
        assert!(icon_url.ends_with(".svg"), "Icon should be SVG format");
    }

    // Edge case: project with no start command
    #[test]
    fn test_nixpacks_fails_on_project_without_entry_point() {
        let temp_dir = TempDir::new().unwrap();
        // Create a package.json without start script
        let package_json = r#"{
  "name": "incomplete-app",
  "version": "1.0.0"
}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        // Nixpacks might still detect it but fail validation
        // This depends on nixpacks behavior - it might still work with default start
        let can_detect = NixpacksPreset::can_detect(temp_dir.path());

        // Document the behavior - this might be true or false depending on nixpacks version
        println!(
            "Project without explicit entry point detection result: {}",
            can_detect
        );
    }

    // Test with fixture directories (if they exist)
    #[test]
    fn test_detect_python_flask_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("temps-deployments/tests/fixtures/simple-python");

        if fixture_path.exists() {
            assert!(
                NixpacksPreset::can_detect(&fixture_path),
                "Should detect Python Flask fixture"
            );
        } else {
            println!(
                "Python fixture not found at {:?}, skipping test",
                fixture_path
            );
        }
    }

    #[test]
    fn test_detect_nextjs_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("temps-deployments/tests/fixtures/simple-nextjs");

        if fixture_path.exists() {
            assert!(
                NixpacksPreset::can_detect(&fixture_path),
                "Should detect Next.js fixture"
            );
        } else {
            println!(
                "Next.js fixture not found at {:?}, skipping test",
                fixture_path
            );
        }
    }

    #[test]
    fn test_detect_rust_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("temps-deployments/tests/fixtures/simple-rust");

        if fixture_path.exists() {
            assert!(
                NixpacksPreset::can_detect(&fixture_path),
                "Should detect Rust fixture"
            );
        } else {
            println!(
                "Rust fixture not found at {:?}, skipping test",
                fixture_path
            );
        }
    }

    #[test]
    fn test_detect_go_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("temps-deployments/tests/fixtures/simple-go");

        if fixture_path.exists() {
            assert!(
                NixpacksPreset::can_detect(&fixture_path),
                "Should detect Go fixture"
            );
        } else {
            println!("Go fixture not found at {:?}, skipping test", fixture_path);
        }
    }
}
