//! Autopack preset — auto-detecting builds via the autopack crates.
//!
//! Autopack is a library, so unlike the builder it replaced nothing is written
//! into the build context and no external binary is invoked: this asks it for a
//! Dockerfile and hands the string back.
//!
//! [`render_or_explain`] is shared with the legacy `nixpacks*` preset slugs,
//! which now build through autopack too.

use std::path::Path;

use async_trait::async_trait;
use autopack_core::{analyze, App, Environment};
use autopack_dockerfile::to_dockerfile;
use tracing::{debug, info, warn};

use crate::{DockerfileConfig, DockerfileWithArgs, Preset, ProjectType};

/// Builds any application autopack recognises.
#[derive(Debug, Clone, Copy, Default)]
pub struct AutopackPreset;

impl AutopackPreset {
    /// Create the preset.
    pub fn new() -> Self {
        Self
    }
}

/// Run autopack over `config` and render a Dockerfile.
///
/// `provider` forces a specific autopack provider; `None` auto-detects.
///
/// Errors are returned as a message rather than a panic: a build that fails
/// with an explanation is recoverable, and the caller renders it to the
/// deployment log.
pub(crate) fn render(
    config: &DockerfileConfig<'_>,
    provider: Option<&str>,
) -> Result<DockerfileWithArgs, String> {
    // Autopack's Dockerfiles use cache and secret mounts, which the classic
    // builder cannot parse. Refusing here names the problem; emitting the
    // Dockerfile anyway fails several minutes later with a syntax error that
    // points at a line the user never wrote.
    if !config.use_buildkit {
        return Err(
            "autopack requires BuildKit — its Dockerfiles use cache and secret mounts. \
             Enable BuildKit for this build (the deployment pipeline does so by default; \
             `temps build` needs `--buildkit`)."
                .to_string(),
        );
    }

    let app = App::new(config.local_path).map_err(|e| e.to_string())?;

    // Build the environment explicitly. Inheriting the server's process
    // environment would let a variable on the control plane change how a
    // user's application builds.
    let mut env = Environment::new();

    for pair in config.build_vars.into_iter().flatten() {
        if let Some((key, value)) = pair.split_once('=') {
            env.set(key, value);
        }
    }

    // Map the platform's own build settings onto autopack's configuration
    // surface, so the existing UI keeps working unchanged.
    if let Some(command) = config.install_command {
        env.set("AUTOPACK_INSTALL_CMD", command);
    }
    if let Some(command) = config.build_command {
        env.set("AUTOPACK_BUILD_CMD", command);
    }
    if let Some(dir) = config.output_dir {
        env.set("AUTOPACK_STATIC_DIR", dir);
    }
    if let Some(provider) = provider {
        env.set("AUTOPACK_PROVIDER", provider);
    }

    let analysis =
        analyze(&app, &env, &autopack_providers::registry()).map_err(|e| e.to_string())?;

    info!(
        provider = %analysis.provider,
        start_command = ?analysis.plan.deploy.start_command,
        "autopack analysed the application"
    );

    // Anything a compatibility translation could not carry over, or a start
    // command that will not survive being taken literally, surfaces here rather
    // than becoming a mysterious runtime failure.
    for (key, value) in &analysis.metadata {
        if key.starts_with("configNote") {
            warn!("autopack: {value}");
        } else {
            debug!("autopack: {key} = {value}");
        }
    }

    let dockerfile = to_dockerfile(&analysis.plan).map_err(|e| e.to_string())?;
    Ok(DockerfileWithArgs::new(dockerfile))
}

/// Render, or fall back to a Dockerfile that fails loudly with the reason.
///
/// The trait cannot return an error, and returning an empty or plausible
/// Dockerfile would turn a detection failure into a confusing runtime failure
/// several minutes later — or, worse, an image that builds and then exits.
pub(crate) fn render_or_explain(
    config: &DockerfileConfig<'_>,
    provider: Option<&str>,
) -> DockerfileWithArgs {
    match render(config, provider) {
        Ok(dockerfile) => dockerfile,
        Err(message) => {
            warn!("autopack could not plan this application: {message}");
            DockerfileWithArgs::new(format!(
                "# autopack could not plan this application.\n\
                 #\n\
                 # {}\n\
                 FROM debian:bookworm-slim\n\
                 RUN echo {} >&2 && exit 1\n",
                message.replace('\n', "\n# "),
                shell_quote(&message)
            ))
        }
    }
}

/// Quote a message for safe interpolation into a shell command.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[async_trait]
impl Preset for AutopackPreset {
    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Autopack (auto-detect)".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/autopack.svg".to_string()
    }

    fn description(&self) -> String {
        "Detects the language and framework automatically and builds an \
         unprivileged, minimal image. Supports 24 ecosystems and reads \
         existing nixpacks.toml or railpack.json configuration."
            .to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        render_or_explain(&config, None)
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        let mut config = DockerfileConfig::new(local_path, local_path, "app");
        config.use_buildkit = true;
        render_or_explain(&config, None)
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec![".".to_string()]
    }

    fn slug(&self) -> String {
        "autopack".to_string()
    }

    fn default_port(&self) -> u16 {
        3000
    }
}

impl std::fmt::Display for AutopackPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Autopack")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, contents).unwrap();
        }
        dir
    }

    fn node_app() -> tempfile::TempDir {
        fixture(&[
            ("package.json", r#"{"scripts":{"start":"node server.js"}}"#),
            ("server.js", ""),
        ])
    }

    fn buildkit_config<'a>(path: &'a Path) -> DockerfileConfig<'a> {
        let mut config = DockerfileConfig::new(path, path, "app");
        config.use_buildkit = true;
        config
    }

    #[tokio::test]
    async fn renders_a_dockerfile_for_a_node_app() {
        let dir = node_app();
        let result = AutopackPreset::new()
            .dockerfile_with_build_dir(dir.path())
            .await;

        assert!(result.content.starts_with("# syntax="), "{}", result.content);
        assert!(result.content.contains("node server.js"));
    }

    #[tokio::test]
    async fn an_unrecognised_app_produces_a_dockerfile_that_fails_loudly() {
        // Returning something that builds would hide the real problem until
        // the container refuses to start.
        let dir = fixture(&[("notes.txt", "nothing to build here")]);

        let result = AutopackPreset::new()
            .dockerfile_with_build_dir(dir.path())
            .await;

        assert!(result.content.contains("exit 1"), "{}", result.content);
        assert!(result.content.contains("autopack could not plan"));
    }

    #[tokio::test]
    async fn platform_build_settings_reach_autopack() {
        let dir = node_app();
        let mut config = buildkit_config(dir.path());
        config.build_command = Some("npm run build:prod");

        let result = AutopackPreset::new().dockerfile(config).await;
        assert!(result.content.contains("build:prod"), "{}", result.content);
    }

    #[tokio::test]
    async fn a_build_without_buildkit_is_refused_by_name() {
        // The classic builder cannot parse `--mount`, and the error it gives
        // points at a generated line rather than at the missing feature.
        let dir = node_app();
        let config = DockerfileConfig::new(dir.path(), dir.path(), "app");
        assert!(!config.use_buildkit, "the default must stay off");

        let result = AutopackPreset::new().dockerfile(config).await;
        assert!(result.content.contains("BuildKit"), "{}", result.content);
        assert!(result.content.contains("exit 1"));
    }

    #[test]
    fn forcing_a_provider_overrides_detection() {
        // A repository that looks like two things must build as the one the
        // user picked, not the one that happens to detect first.
        let dir = fixture(&[("main.go", "package main\nfunc main() {}\n"), ("go.mod", "module x\n\ngo 1.22\n")]);
        let config = buildkit_config(dir.path());

        let forced = render(&config, Some("go")).expect("go provider");
        assert!(forced.content.contains("go build"), "{}", forced.content);
    }
}
