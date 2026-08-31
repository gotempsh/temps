// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The legacy `nixpacks*` preset slugs, now built by autopack.
//!
//! The Nixpacks library is gone; these slugs remain. Projects created before
//! the switch have `preset = 'nixpacks'` and a [`NixpacksConfig`] persisted in
//! the database, and a deployment of one of them must keep working without the
//! user touching anything — so the slugs, labels, icons and stored config shape
//! are all preserved, and only the engine underneath changed.
//!
//! New projects should use the `autopack` slug ([`crate::AutopackPreset`]).
//! This module is the compatibility surface, not the entry point.
//!
//! Two behaviours are deliberately kept rather than "cleaned up":
//!
//! * a persisted `nixpacks_config` TOML is still honoured — autopack reads the
//!   Nixpacks schema in compatibility mode, and reports anything it could not
//!   translate instead of dropping it silently;
//! * the provider stored against a project still forces that language, so a
//!   polyglot repository pinned to `python` does not silently start building
//!   as `node` because detection order differs.

use super::autopack_preset::render_or_explain;
use crate::{DockerfileConfig, DockerfileWithArgs, Preset, ProjectType};
use async_trait::async_trait;
use autopack_core::compat::nixpacks::NixpacksConfig as CompatNixpacksConfig;
use autopack_core::{App, Environment};
use std::path::Path;
use temps_entities::preset::NixpacksConfig;
pub use temps_entities::preset::NixpacksProvider;
use tracing::{debug, warn};

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
        NixpacksProvider::Auto => "/presets/autopack.svg",
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
            "Node.js apps — Next, Nuxt, Vue, SvelteKit, Astro, Remix, Express, and more"
        }
        NixpacksProvider::Python => "Python web applications (Django, Flask, FastAPI, etc.)",
        NixpacksProvider::Rust => "Rust web applications and services",
        NixpacksProvider::Go => "Go web applications and services",
        NixpacksProvider::Java => "Java web applications (Spring Boot, Micronaut, Quarkus, etc.)",
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

/// The autopack provider that builds what this Nixpacks provider used to.
///
/// `None` means "let autopack detect", which is right for `Auto` — and is also
/// the only honest answer for a provider autopack does not implement. Forcing
/// an id autopack does not know would fail the build outright; falling back to
/// detection at least gives the app a chance, and the caller logs the gap.
fn autopack_provider(provider: NixpacksProvider) -> Option<&'static str> {
    match provider {
        NixpacksProvider::Auto => None,
        NixpacksProvider::Node => Some("node"),
        NixpacksProvider::Python => Some("python"),
        NixpacksProvider::Rust => Some("rust"),
        NixpacksProvider::Go => Some("go"),
        NixpacksProvider::Java => Some("java"),
        NixpacksProvider::Php => Some("php"),
        NixpacksProvider::Ruby => Some("ruby"),
        NixpacksProvider::Deno => Some("deno"),
        NixpacksProvider::Elixir => Some("elixir"),
        // Nixpacks split .NET by language; autopack has one provider for both.
        NixpacksProvider::CSharp | NixpacksProvider::FSharp => Some("dotnet"),
        NixpacksProvider::Dart => Some("dart"),
        NixpacksProvider::Swift => Some("swift"),
        NixpacksProvider::Zig => Some("zig"),
        NixpacksProvider::Scala => Some("scala"),
        NixpacksProvider::Haskell => Some("haskell"),
        NixpacksProvider::Clojure => Some("clojure"),
        NixpacksProvider::Crystal => Some("crystal"),
        NixpacksProvider::Cobol => Some("cobol"),
        NixpacksProvider::Gleam => Some("gleam"),
        NixpacksProvider::Lunatic => Some("lunatic"),
        // Nixpacks' Scheme support was its Haunt static-site generator; autopack
        // has no equivalent, so detection decides (usually `static` or `shell`).
        NixpacksProvider::Scheme => None,
        NixpacksProvider::Static => Some("static"),
    }
}

/// Render a byte offset as `line L, column C`, counting from one.
///
/// Positions are derived rather than taken from the error's own rendering,
/// which embeds the source text.
fn line_and_column(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let consumed = &source[..offset];
    let line = consumed.matches('\n').count() + 1;
    let column = consumed
        .rfind('\n')
        .map_or(offset, |newline| offset - newline - 1)
        + 1;
    format!("line {line}, column {column}")
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

    /// Reject a stored `nixpacks_config` that no longer parses.
    ///
    /// The check runs against autopack's compatibility reader — the same code
    /// that will read the file at build time — so validation cannot pass for
    /// something the build then refuses.
    ///
    /// The returned message carries the *position* of the error and nothing
    /// else. It reaches an API response body and the logs, and a
    /// `nixpacks_config` can hold secrets: `toml`'s own error Display renders
    /// the offending source line, and `.message()` names the offending key on
    /// an unknown-field error. Neither may cross that boundary, so the detail
    /// is logged server-side instead of returned.
    pub(super) fn validate_config(
        config: &NixpacksConfig,
    ) -> Result<(), crate::PresetResolutionError> {
        if let Some(value) = config.nixpacks_config.as_deref() {
            if let Err(error) = toml::from_str::<CompatNixpacksConfig>(value) {
                debug!("stored nixpacks.toml did not parse: {error}");
                let where_ = error
                    .span()
                    .map(|span| format!(" at {}", line_and_column(value, span.start)))
                    .unwrap_or_default();
                return Err(crate::PresetResolutionError::InvalidConfig {
                    slug: "nixpacks".to_string(),
                    reason: format!(
                        "failed to parse Nixpacks TOML{where_}; \
                         verify its syntax and supported fields"
                    ),
                });
            }
        }
        Ok(())
    }

    fn display_provider(&self) -> NixpacksProvider {
        match self.config.providers.as_slice() {
            [provider] => *provider,
            _ => NixpacksProvider::Auto,
        }
    }

    /// The autopack provider id to force for this preset, if any.
    fn forced_provider(&self) -> Option<&'static str> {
        let provider = self.display_provider();
        let mapped = autopack_provider(provider);
        if mapped.is_none() && provider != NixpacksProvider::Auto {
            warn!(
                provider = provider_name(provider),
                "autopack has no provider for this language; falling back to auto-detection"
            );
        }
        mapped
    }

    /// Whether autopack can plan a build for this project at all.
    pub fn can_detect(path: &Path) -> bool {
        Self::detect_provider_id(path).is_some()
    }

    /// The autopack provider that claims this project, if any.
    fn detect_provider_id(path: &Path) -> Option<String> {
        let app = match App::new(path) {
            Ok(app) => app,
            Err(error) => {
                debug!("autopack: could not read {path:?}: {error}");
                return None;
            }
        };
        let env = Environment::new();
        match autopack_providers::registry().detect(&app, &env) {
            Ok(Some(provider)) => Some(provider.id().to_string()),
            Ok(None) => None,
            Err(error) => {
                debug!("autopack: detection failed for {path:?}: {error}");
                None
            }
        }
    }

    /// Which of the selectable providers can build this project.
    ///
    /// Autopack resolves exactly one provider per project rather than ranking
    /// candidates, so this reports that one — the UI uses it to preselect a
    /// language, and offering several would imply a choice that does not exist.
    pub fn detect_available_providers(path: &Path) -> Vec<NixpacksProvider> {
        let Some(detected) = Self::detect_provider_id(path) else {
            return Vec::new();
        };
        [
            NixpacksProvider::Node,
            NixpacksProvider::Python,
            NixpacksProvider::Rust,
            NixpacksProvider::Go,
            NixpacksProvider::Java,
            NixpacksProvider::Php,
            NixpacksProvider::Ruby,
            NixpacksProvider::Deno,
            NixpacksProvider::Elixir,
            NixpacksProvider::CSharp,
            NixpacksProvider::Dart,
            NixpacksProvider::Swift,
            NixpacksProvider::Zig,
            NixpacksProvider::Scala,
            NixpacksProvider::Haskell,
            NixpacksProvider::Clojure,
            NixpacksProvider::Crystal,
            NixpacksProvider::Cobol,
            NixpacksProvider::Gleam,
            NixpacksProvider::Lunatic,
            NixpacksProvider::Static,
        ]
        .into_iter()
        .filter(|provider| autopack_provider(*provider) == Some(detected.as_str()))
        .collect()
    }

    /// Apply the persisted Nixpacks TOML, if there is one, as an overlay file
    /// autopack will read in compatibility mode.
    ///
    /// The config lives in the database, not the repository, so it has to be
    /// materialised somewhere autopack can see it. Writing it into the build
    /// context is what the previous implementation did with `.nixpacks/`, and
    /// keeps the semantics identical.
    fn stage_config(&self, local_path: &Path) -> Option<StagedConfig> {
        let contents = self.config.nixpacks_config.as_deref()?;
        // A repository that ships its own file already says what its author
        // wanted; the stored config is the platform's copy of the same thing,
        // and overwriting the file would lose the user's edits.
        let path = local_path.join("nixpacks.toml");
        if path.exists() {
            debug!("nixpacks.toml already present in the repository; leaving it alone");
            return None;
        }
        match std::fs::write(&path, contents) {
            Ok(()) => Some(StagedConfig { path }),
            Err(error) => {
                warn!("could not stage the stored nixpacks.toml: {error}");
                None
            }
        }
    }
}

/// Removes a staged `nixpacks.toml` when the build plan has been generated.
///
/// The build context is a checkout the platform owns, but leaving the file
/// behind would make a later `autopack.json` look like it lost to a stale
/// compatibility file.
struct StagedConfig {
    path: std::path::PathBuf,
}

impl Drop for StagedConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[async_trait]
impl Preset for NixpacksPreset {
    fn project_type(&self) -> ProjectType {
        // The static provider serves a directory of pre-built files with no
        // language runtime (see `autopack_provider`'s "static" mapping) — it
        // must report `Static` so the workflow planner routes it through
        // `DeployStaticJob` (extract files from the built image, serve them
        // straight off disk via temps-proxy) instead of `DeployImageJob`
        // (run the image as a long-lived container). The container path is
        // both unnecessary overhead for static content and actively broken
        // for this specific image: the official `caddy:2-alpine` binary
        // carries a `cap_net_bind_service` file capability, and Temps' own
        // container hardening (`cap_drop: ALL`) makes the kernel refuse to
        // exec a binary whose file capabilities exceed the process's
        // capability bounding set. Rollback/promotion also branch on this
        // (see `temps-deployments/src/services/services.rs`) to skip
        // container redeploy entirely for static presets.
        if self.display_provider() == NixpacksProvider::Static {
            ProjectType::Static
        } else {
            ProjectType::Server
        }
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
            format!("Autopack ({providers})")
        } else {
            format!("Autopack ({})", provider_name(self.display_provider()))
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
        let _staged = self.stage_config(config.local_path);
        render_or_explain(&config, self.forced_provider())
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        let _staged = self.stage_config(local_path);
        let mut config = DockerfileConfig::new(local_path, local_path, "app");
        config.use_buildkit = true;
        render_or_explain(&config, self.forced_provider())
    }

    fn install_command(&self, _local_path: &Path) -> String {
        "# Handled by autopack".to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        "# Handled by autopack".to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // The whole project directory: autopack decides what it needs.
        vec![".".to_string()]
    }

    fn slug(&self) -> String {
        match self.config.providers.as_slice() {
            [provider] => provider.variant_slug().to_string(),
            _ => "nixpacks".to_string(),
        }
    }

    fn needs_container_build(&self) -> bool {
        // The static provider has nothing to build — it serves the checked-out
        // source directly. Reporting `false` here (rather than the usual
        // `static_output_dir()` override) means the workflow planner skips
        // Docker/autopack entirely for this preset: no image, no Caddy, no
        // container ever runs. `static_output_dir()` stays `None` (the
        // default) since there is no build-tool output directory inside an
        // image to extract from.
        self.display_provider() != NixpacksProvider::Static
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

    fn project(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (path, contents) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, contents).unwrap();
        }
        dir
    }

    fn nodejs_project() -> TempDir {
        project(&[
            (
                "package.json",
                r#"{"name":"test-app","scripts":{"start":"node index.js"},"dependencies":{"express":"^4.18.0"}}"#,
            ),
            ("index.js", "console.log('Hello World')"),
        ])
    }

    fn python_project() -> TempDir {
        project(&[
            ("requirements.txt", "flask==2.0.0"),
            ("main.py", "print('hello')"),
        ])
    }

    async fn dockerfile_for(preset: &NixpacksPreset, dir: &Path) -> String {
        let mut config = DockerfileConfig::new(dir, dir, "test-project");
        config.use_buildkit = true;
        preset.dockerfile(config).await.content
    }

    #[test]
    fn detects_the_common_languages() {
        for (label, dir) in [
            ("node", nodejs_project()),
            ("python", python_project()),
            (
                "go",
                project(&[("go.mod", "module x\n\ngo 1.22\n"), ("main.go", "package main\nfunc main() {}\n")]),
            ),
            (
                "rust",
                project(&[
                    ("Cargo.toml", "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
                    ("src/main.rs", "fn main() {}"),
                ]),
            ),
            ("dart", project(&[("pubspec.yaml", "name: x\n")])),
        ] {
            assert!(
                NixpacksPreset::can_detect(dir.path()),
                "should detect the {label} project"
            );
        }
    }

    #[test]
    fn a_directory_with_nothing_to_build_is_not_detected() {
        let dir = project(&[("README.md", "# nothing here")]);
        assert!(!NixpacksPreset::can_detect(dir.path()));
    }

    #[test]
    fn detection_reports_the_matching_provider() {
        let dir = python_project();
        assert_eq!(
            NixpacksPreset::detect_available_providers(dir.path()),
            vec![NixpacksProvider::Python]
        );
    }

    #[tokio::test]
    async fn generates_a_dockerfile_for_an_auto_detected_project() {
        let dir = python_project();
        let content = dockerfile_for(&NixpacksPreset::auto(), dir.path()).await;

        assert!(content.starts_with("# syntax="), "{content}");
        assert!(!content.contains("autopack could not plan"), "{content}");
    }

    #[tokio::test]
    async fn a_stored_provider_forces_that_language() {
        // A repository can look like more than one thing. If the project was
        // pinned to a language, detection order must not override it.
        let dir = project(&[
            ("package.json", r#"{"scripts":{"start":"node index.js"}}"#),
            ("index.js", ""),
            ("requirements.txt", "flask==2.0.0"),
            ("main.py", "print('hi')"),
        ]);

        let content = dockerfile_for(&NixpacksPreset::new(NixpacksProvider::Python), dir.path()).await;
        assert!(content.contains("pip"), "{content}");
    }

    #[test]
    fn slugs_and_stored_preset_are_unchanged() {
        // These are persisted. Changing either orphans existing projects.
        assert_eq!(NixpacksPreset::auto().slug(), "nixpacks");
        assert_eq!(
            NixpacksPreset::new(NixpacksProvider::Node).slug(),
            NixpacksProvider::Node.variant_slug()
        );
        assert_eq!(
            NixpacksPreset::auto().stored_preset(),
            Some(temps_entities::preset::Preset::Nixpacks)
        );
    }

    #[test]
    fn the_static_provider_skips_docker_entirely() {
        // The workflow planner (`plan_git_deployment`) picks the source-only
        // deployment path (`DeployStaticFromSourceJob`: no Docker, no
        // autopack, no container) over the container path purely based on
        // `needs_container_build()` being `false`. Getting this wrong for the
        // static provider means an image gets built (and possibly *run*)
        // for content that's just files to serve — wasted build time at
        // best, and at worst a crash-looping container, since
        // `caddy:2-alpine`'s binary carries a file capability Temps' hardened
        // `cap_drop: ALL` containers cannot grant.
        let preset = NixpacksPreset::new(NixpacksProvider::Static);
        assert_eq!(preset.project_type(), ProjectType::Static);
        assert!(!preset.needs_container_build());
        // No build-tool output dir inside an image — there is no image.
        assert_eq!(preset.static_output_dir(), None);

        // Rollback/promotion (`temps-deployments/src/services/services.rs`)
        // also key off `project_type()` to skip container redeploy for static
        // presets — every other provider must keep reporting `Server` and
        // still needing a container build.
        for provider in [
            NixpacksProvider::Node,
            NixpacksProvider::Python,
            NixpacksProvider::Rust,
            NixpacksProvider::Go,
        ] {
            let preset = NixpacksPreset::new(provider);
            assert_eq!(preset.project_type(), ProjectType::Server, "{provider:?}");
            assert!(preset.needs_container_build(), "{provider:?}");
        }

        // Multi-provider / auto-detect configurations have no single static
        // root to report — must not misreport `Static`.
        assert_eq!(NixpacksPreset::auto().project_type(), ProjectType::Server);
        assert!(NixpacksPreset::auto().needs_container_build());
    }

    #[tokio::test]
    async fn a_persisted_nixpacks_toml_reaches_the_build() {
        // The config lives in the database, so it has to be materialised for
        // autopack's compatibility reader to see it.
        let dir = nodejs_project();
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: Some("[start]\ncmd = \"node custom-entry.js\"\n".to_string()),
            ..Default::default()
        });

        let content = dockerfile_for(&preset, dir.path()).await;
        assert!(content.contains("node custom-entry.js"), "{content}");
    }

    #[tokio::test]
    async fn staging_the_stored_config_leaves_no_file_behind() {
        // A leftover nixpacks.toml would beat a later autopack.json.
        let dir = nodejs_project();
        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: Some("[start]\ncmd = \"node index.js\"\n".to_string()),
            ..Default::default()
        });

        dockerfile_for(&preset, dir.path()).await;
        assert!(!dir.path().join("nixpacks.toml").exists());
    }

    #[tokio::test]
    async fn a_repository_nixpacks_toml_wins_over_the_stored_one() {
        // The file in the repository is the one the author is editing.
        let dir = nodejs_project();
        fs::write(
            dir.path().join("nixpacks.toml"),
            "[start]\ncmd = \"node from-repo.js\"\n",
        )
        .unwrap();

        let preset = NixpacksPreset::from_config(NixpacksConfig {
            nixpacks_config: Some("[start]\ncmd = \"node from-database.js\"\n".to_string()),
            ..Default::default()
        });

        let content = dockerfile_for(&preset, dir.path()).await;
        assert!(content.contains("node from-repo.js"), "{content}");
        assert!(
            dir.path().join("nixpacks.toml").exists(),
            "the repository's own file must survive"
        );
    }

    #[test]
    fn invalid_stored_toml_is_rejected_before_the_build() {
        let result = NixpacksPreset::validate_config(&NixpacksConfig {
            nixpacks_config: Some("this is not = valid = toml".to_string()),
            ..Default::default()
        });
        assert!(result.is_err());
    }

    #[test]
    fn a_validation_error_never_echoes_the_config_back() {
        // This message reaches an API response body and the logs, and the
        // config can hold secrets. `toml`'s own error Display renders the
        // offending source line, so interpolating it leaks the value — which
        // is exactly what happened here once.
        let result = NixpacksPreset::validate_config(&NixpacksConfig {
            nixpacks_config: Some("secret_token = [\"do-not-echo\"".to_string()),
            ..Default::default()
        });
        let message = result.unwrap_err().to_string();
        assert!(
            !message.contains("do-not-echo"),
            "the error echoed the config: {message}"
        );
        assert!(!message.contains("secret_token"), "{message}");
        // The position is still there, so the user can find the problem.
        assert!(message.contains("line 1, column 30"), "{message}");
    }

    #[test]
    fn the_reported_position_counts_lines_and_columns_from_one() {
        assert_eq!(line_and_column("abc", 0), "line 1, column 1");
        assert_eq!(line_and_column("abc\ndef", 5), "line 2, column 2");
        // An offset at end-of-input is what an unterminated array produces.
        assert_eq!(line_and_column("ab", 99), "line 1, column 3");
    }

    #[test]
    fn valid_stored_toml_passes_validation() {
        let result = NixpacksPreset::validate_config(&NixpacksConfig {
            nixpacks_config: Some("[start]\ncmd = \"./server\"\n".to_string()),
            ..Default::default()
        });
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn dotnet_providers_share_one_autopack_provider() {
        assert_eq!(autopack_provider(NixpacksProvider::CSharp), Some("dotnet"));
        assert_eq!(autopack_provider(NixpacksProvider::FSharp), Some("dotnet"));
    }

    #[test]
    fn a_language_autopack_lacks_falls_back_to_detection() {
        // Forcing an id autopack does not know would fail the build outright.
        assert_eq!(autopack_provider(NixpacksProvider::Scheme), None);
        assert_eq!(
            NixpacksPreset::new(NixpacksProvider::Scheme).forced_provider(),
            None
        );
    }

    #[test]
    fn every_selectable_provider_maps_to_a_real_autopack_provider() {
        // A typo here is a build that fails with "unknown provider" for one
        // language only, which is exactly the kind of thing nobody notices.
        let registry = autopack_providers::registry();
        for provider in [
            NixpacksProvider::Node,
            NixpacksProvider::Python,
            NixpacksProvider::Rust,
            NixpacksProvider::Go,
            NixpacksProvider::Java,
            NixpacksProvider::Php,
            NixpacksProvider::Ruby,
            NixpacksProvider::Deno,
            NixpacksProvider::Elixir,
            NixpacksProvider::CSharp,
            NixpacksProvider::FSharp,
            NixpacksProvider::Dart,
            NixpacksProvider::Swift,
            NixpacksProvider::Zig,
            NixpacksProvider::Scala,
            NixpacksProvider::Haskell,
            NixpacksProvider::Clojure,
            NixpacksProvider::Crystal,
            NixpacksProvider::Cobol,
            NixpacksProvider::Gleam,
            NixpacksProvider::Lunatic,
            NixpacksProvider::Static,
        ] {
            let id = autopack_provider(provider)
                .unwrap_or_else(|| panic!("{provider:?} should map to a provider"));
            assert!(
                registry.get(id).is_some(),
                "{provider:?} maps to `{id}`, which autopack does not register"
            );
        }
    }

    #[tokio::test]
    async fn a_build_without_buildkit_is_refused_by_name() {
        let dir = nodejs_project();
        let config = DockerfileConfig::new(dir.path(), dir.path(), "test");
        let content = NixpacksPreset::auto().dockerfile(config).await.content;
        assert!(content.contains("BuildKit"), "{content}");
    }

    #[tokio::test]
    async fn an_unbuildable_project_fails_loudly_rather_than_producing_a_running_image() {
        // The previous implementation emitted `FROM alpine` + `COPY . .` with no
        // CMD, which builds, deploys, and then exits immediately.
        let dir = project(&[("README.md", "# nothing here")]);
        let content = dockerfile_for(&NixpacksPreset::auto(), dir.path()).await;
        assert!(content.contains("exit 1"), "{content}");
    }
}
