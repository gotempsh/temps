// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Behaviour of the stored preset types: framework metadata, parsing a stored
//! preset configuration and Nixpacks provider names.
//!
//! The types themselves stay in `temps_entities::preset` because entity models
//! store them as typed columns, so these are extension traits.
use temps_entities::preset::*;

pub trait StoredPresetExt {
    /// Get the human-readable display name
    fn display_name(&self) -> &'static str;

    /// Get the language/runtime for this preset
    fn language(&self) -> &'static str;

    /// Check if this preset supports static site generation
    fn is_static_capable(&self) -> bool;

    /// Check if this preset requires a runtime server
    fn requires_server(&self) -> bool;

    /// Get the default exposed port for this preset
    ///
    /// Returns the typical port that this framework/runtime listens on.
    /// Returns None for static presets that don't have a runtime server.
    fn exposed_port(&self) -> Option<u16>;

    /// Get the icon URL for this preset (logo or framework icon)
    fn icon_url(&self) -> Option<&'static str>;

    /// Get the project type category for this preset
    fn project_type(&self) -> &'static str;
}

impl StoredPresetExt for Preset {
    fn display_name(&self) -> &'static str {
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

    fn language(&self) -> &'static str {
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

    fn is_static_capable(&self) -> bool {
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

    fn requires_server(&self) -> bool {
        !matches!(self, Preset::Static)
    }

    fn exposed_port(&self) -> Option<u16> {
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

    fn icon_url(&self) -> Option<&'static str> {
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

    fn project_type(&self) -> &'static str {
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
}

pub trait StoredPresetConfigExt: Sized {
    /// Create a default configuration for a given preset
    fn default_for_preset(preset: Preset) -> Self;

    /// Parse a JSON value into a PresetConfig for the given preset.
    ///
    /// The JSON value should contain the config fields for the preset
    /// (e.g., `{"dockerfilePath": "docker/Dockerfile"}` for Dockerfile preset).
    /// The `preset` tag is added automatically based on the provided preset enum.
    fn parse_for_preset(preset: &Preset, value: &serde_json::Value) -> Result<Self, String>;

    /// Get the preset type from this configuration
    fn preset_type(&self) -> Preset;
}

impl StoredPresetConfigExt for PresetConfig {
    fn default_for_preset(preset: Preset) -> Self {
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

    fn parse_for_preset(preset: &Preset, value: &serde_json::Value) -> Result<Self, String> {
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

    fn preset_type(&self) -> Preset {
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

pub trait NixpacksProviderExt {
    /// Provider identifier expected by Nixpacks build plans.
    fn nixpacks_name(self) -> &'static str;

    fn variant_slug(self) -> &'static str;
}

impl NixpacksProviderExt for NixpacksProvider {
    fn nixpacks_name(self) -> &'static str {
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

    fn variant_slug(self) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dockerfile_runtime_defaults_and_entrypoint_reset_are_distinct() {
        for json in [
            serde_json::json!({}),
            serde_json::json!({"command": null, "entrypoint": null}),
        ] {
            let config: DockerfileConfig = serde_json::from_value(json).unwrap();
            assert_eq!(config.runtime, ContainerRuntimeOverrides::default());
        }
        let config: DockerfileConfig =
            serde_json::from_value(serde_json::json!({"entrypoint": []})).unwrap();
        assert_eq!(config.runtime.entrypoint, Some(vec![]));
        assert_eq!(
            serde_json::to_value(config).unwrap()["entrypoint"],
            serde_json::json!([])
        );
        assert!(PresetConfig::parse_for_preset(
            &Preset::Dockerfile,
            &serde_json::json!({"command": "python worker.py"})
        )
        .is_err());
    }

    #[test]
    fn dockerfile_config_preserves_runtime_overrides() {
        let input = serde_json::json!({
            "dockerfilePath": "Dockerfile",
            "command": ["python", "-m", "worker", "argument with spaces"],
            "entrypoint": ["/bin/sh", "-c"]
        });
        let config = PresetConfig::parse_for_preset(&Preset::Dockerfile, &input).unwrap();
        let PresetConfig::Dockerfile(config) = config else {
            panic!("expected Dockerfile configuration");
        };
        let stored = serde_json::to_value(config).unwrap();
        assert_eq!(stored["command"], input["command"]);
        assert_eq!(stored["entrypoint"], input["entrypoint"]);
    }

    #[test]
    fn test_docker_compose_config_persists_compose_services() {
        let value = serde_json::json!({
            "preset": "docker-compose",
            "composePath": "compose.yml",
            "excludedServices": ["postgres"],
            "composeServices": [
                {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
                {"name": "hub", "image": "ghcr.io/example/hub:latest", "looksLikeDatabase": false}
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

    #[test]
    fn test_preset_display_name() {
        assert_eq!(Preset::NextJs.display_name(), "Next.js");
        assert_eq!(Preset::Rails.display_name(), "Ruby on Rails");
    }

    #[test]
    fn preset_runtime_metadata() {
        assert!(Preset::Vite.is_static_capable());
        assert!(!Preset::FastApi.is_static_capable());
        assert!(Preset::FastApi.requires_server());
        assert!(!Preset::Static.requires_server());
        assert_eq!(Preset::Vite.exposed_port(), Some(5173));
        assert_eq!(Preset::Static.exposed_port(), None);
        assert_eq!(
            Preset::Go.icon_url(),
            Some("https://cdn.simpleicons.org/go/00ADD8")
        );
        assert_eq!(Preset::Autopack.icon_url(), None);
        assert_eq!(Preset::NextJs.project_type(), "fullstack");
        assert_eq!(Preset::Dockerfile.project_type(), "container");
    }

    #[test]
    fn default_config_of_every_preset_reports_that_preset() {
        for preset in Preset::all() {
            assert_eq!(PresetConfig::default_for_preset(preset).preset_type(), preset);
        }
    }

    #[test]
    fn autopack_has_no_config_of_its_own_and_defaults_to_nixpacks() {
        assert_eq!(
            PresetConfig::default_for_preset(Preset::Autopack).preset_type(),
            Preset::Nixpacks
        );
    }

    #[test]
    fn nixpacks_provider_names_and_slugs() {
        assert_eq!(NixpacksProvider::Auto.nixpacks_name(), "...");
        assert_eq!(NixpacksProvider::CSharp.nixpacks_name(), "c#");
        assert_eq!(NixpacksProvider::Static.nixpacks_name(), "staticfile");
        assert_eq!(NixpacksProvider::Auto.variant_slug(), "nixpacks");
        assert_eq!(NixpacksProvider::CSharp.variant_slug(), "nixpacks-csharp");
    }
}
