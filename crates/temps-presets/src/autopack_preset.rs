// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use autopack_core::{analyze, App, Config, Environment, Procfile};
use autopack_dockerfile::to_dockerfile;
use tracing::{debug, info, warn};

use crate::{BuildPlanFailure, DockerfileConfig, DockerfileWithArgs, Preset, ProjectType};

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

    let workspace_app = if provider.is_none() || provider == Some("node") {
        node_app_directory(config)?
    } else {
        None
    };
    let analysis_root = if workspace_app.is_some() {
        config.root_local_path
    } else {
        config.local_path
    };
    let app = App::new(analysis_root).map_err(|e| e.to_string())?;

    // Build the environment explicitly. Inheriting the server's process
    // environment would let a variable on the control plane change how a
    // user's application builds.
    let mut env = Environment::new();

    // `KEY=VALUE` entries configure autopack itself (`AUTOPACK_*`). A bare
    // name is a project variable whose value the build receives as a
    // `--build-arg`: autopack declares it as an `ARG` in its build step, after
    // dependencies are installed, which is where a framework inlines
    // `VITE_*`/`NEXT_PUBLIC_*`/`PUBLIC_*` values into its output.
    for entry in config.build_vars.into_iter().flatten() {
        match entry.split_once('=') {
            Some((key, value)) => {
                env.set(key, value);
            }
            None if is_build_arg_name(entry) => {
                env.add_build_arg(entry.as_str());
            }
            // Autopack rejects a plan whose `ARG` name could break out of the
            // line, so a variable named e.g. `my-var` would otherwise fail the
            // whole build. It still reaches the running container.
            None => warn!(
                variable = %entry.escape_debug(),
                "autopack: not passing variable to the build step: build arguments must be \
                 letters, digits and underscores, not starting with a digit"
            ),
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

    if let Some(relative) = &workspace_app {
        // Plan the selected server separately to retain its entry point and framework
        // defaults, but install and build against the complete root workspace.
        let selected = App::new(config.local_path).map_err(|e| e.to_string())?;
        let mut selected_env = env.clone();
        selected_env.set("AUTOPACK_PROVIDER", "node");
        let selected_analysis = analyze(&selected, &selected_env, &autopack_providers::registry())
            .map_err(|e| e.to_string())?;
        let root_package: autopack_providers::node::PackageJson = app
            .read_json_opt("package.json")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let manager = autopack_providers::node::PackageManager::detect(&app, &root_package);
        let selected_package: autopack_providers::node::PackageJson = selected
            .read_json("package.json")
            .map_err(|e| e.to_string())?;
        let inferred_start = selected_analysis
            .plan
            .deploy
            .start_command
            .as_deref()
            .ok_or_else(|| {
                format!("No start command found for workspace application {relative}")
            })?;
        let selected_manager =
            autopack_providers::node::PackageManager::detect(&selected, &selected_package);
        let explicit_start = Config::load(&selected, &selected_env)
            .map_err(|e| e.to_string())?
            .deploy
            .is_some_and(|deploy| deploy.start_command.is_some())
            || Procfile::load(&selected)
                .map_err(|e| e.to_string())?
                .is_some_and(|procfile| procfile.web().is_some());
        // Keep direct starts so the application receives SIGTERM. Yarn Berry
        // needs its launcher to activate the package loader, while compound
        // start scripts need the root manager rather than the app's fallback.
        let start = if explicit_start {
            inferred_start.to_string()
        } else if manager != autopack_providers::node::PackageManager::Pnpm
            && (inferred_start == selected_manager.run_command("start")
                || (manager == autopack_providers::node::PackageManager::YarnBerry
                    && selected_package.script("start") == Some(inferred_start)))
        {
            manager.run_command("start")
        } else if matches!(manager, autopack_providers::node::PackageManager::YarnBerry)
            && inferred_start.starts_with("node ")
        {
            format!("yarn {inferred_start}")
        } else {
            inferred_start.to_string()
        };
        env.set("AUTOPACK_PROVIDER", "node");
        env.set(
            "AUTOPACK_START_CMD",
            format!("cd /app/{relative} && PATH=/app/{relative}/node_modules/.bin:$PATH {start}"),
        );
        if config.build_command.is_none() {
            env.set(
                "AUTOPACK_BUILD_CMD",
                if manager == autopack_providers::node::PackageManager::Pnpm {
                    format!("pnpm --filter './{relative}...' --if-present run build")
                } else if selected_package.script("build").is_some() {
                    format!("cd /app/{relative} && {}", manager.run_command("build"))
                } else {
                    ":".to_string()
                },
            );
        } else if let Some(command) = config.build_command {
            env.set(
                "AUTOPACK_BUILD_CMD",
                format!("cd /app/{relative} && {command}"),
            );
        }
    }
    let registry = autopack_providers::registry();
    // Validate the requested interpreter before planning can mask it with an
    // unrelated missing-start-command error. Resolve the same effective provider
    // as autopack so incidental Ruby files in a Node app do not affect it.
    if app.has_file(".ruby-version") {
        let effective_config =
            autopack_core::Config::load(&app, &env).map_err(|e| e.to_string())?;
        let effective_provider = registry
            .resolve(&app, &env, &effective_config)
            .map_err(|e| e.to_string())?;
        if effective_provider.id() == "ruby" {
            if let Some(requested) = app
                .read_file_opt(".ruby-version")
                .map_err(|e| e.to_string())?
            {
                let version = requested
                    .trim()
                    .strip_prefix("ruby-")
                    .unwrap_or(requested.trim());
                let parts: Vec<_> = version.split('.').collect();
                if !(1..=3).contains(&parts.len())
                    || parts
                        .iter()
                        .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err(format!("Unsupported .ruby-version pin '{}': use an MRI version such as 3.4.11 or ruby-3.4.11; refusing to silently choose a fallback interpreter", requested.trim()));
                }
            }
        }
    }
    let mut analysis = analyze(&app, &env, &registry).map_err(|e| e.to_string())?;
    // A nested application whose build needs the repository root as context
    // (#1342: Python with a root requirements file; Go modules and Cargo
    // crates with sibling dependencies, see `compiled_workspace_app`).
    // Analysis stays on the selected application; install and build copy the
    // repository and run in the application's directory, so relative pip,
    // `replace` and `path` dependencies resolve.
    let nested_prefix = if analysis.provider == "python" {
        python_app_directory(config.root_local_path, config.local_path)?
            .map(|relative| (format!("cd /app/{relative} && "), relative, false))
    } else if let Some(language) =
        crate::CompiledLanguage::from_autopack_provider(&analysis.provider)
    {
        // Rendered into the failing Dockerfile and the build log.
        crate::compiled_workspace_app(config.root_local_path, config.local_path, language)
            .map_err(|error| error.to_string())?
            .map(|compiled| {
                let mut prefix = format!("cd /app/{} && ", compiled.relative);
                match compiled.language {
                    crate::CompiledLanguage::Go if compiled.ignore_go_work => {
                        prefix.push_str("export GOWORK=off && ");
                    }
                    crate::CompiledLanguage::Go => {}
                    // A workspace member's output lands in the workspace's
                    // target directory. Use the cached /app/target either
                    // way, and keep the crate-relative `target/` path the
                    // plan copies the binary from.
                    crate::CompiledLanguage::Cargo => prefix.push_str(
                        "export CARGO_TARGET_DIR=/app/target && if [ ! -L target ]; then rm -rf target \
                         && ln -s /app/target target; fi && ",
                    ),
                }
                (prefix, compiled.relative, true)
            })
    } else {
        None
    };
    if let Some((prefix, relative, compiled_binary)) = nested_prefix {
        for step in &mut analysis.plan.steps {
            if matches!(step.name.as_str(), "install" | "build") {
                for input in &mut step.inputs {
                    if input.local {
                        *input = autopack_core::plan::Layer::local();
                    }
                }
                for command in &mut step.commands {
                    if let autopack_core::plan::Command::Exec(exec) = command {
                        exec.cmd = format!("{prefix}{}", exec.cmd);
                    }
                }
            }
        }
        // A Go or Cargo build's absolute start command is the built binary
        // and runs as planned. Anything else, including an absolute Python
        // launcher such as `/usr/bin/env gunicorn app:app`, names modules and
        // files relative to the application, so it starts in that directory.
        if let Some(start) = &mut analysis.plan.deploy.start_command {
            if !(compiled_binary && start.starts_with('/')) {
                *start = format!("cd /app/{relative} && {start}");
            }
        }
    }

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
    let mut rendered = DockerfileWithArgs::new(dockerfile);
    // The tracing line above only reaches the server log. Carry the same notes
    // back to the build job so the user sees which settings were ignored in the
    // deployment log, next to the build they affect.
    rendered.warnings = untranslated_config_notes(
        analysis
            .metadata
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    Ok(rendered)
}

/// Settings from a compatibility config (`nixpacks.toml`, `railpack.json`)
/// that autopack could not carry over, in the order autopack reported them.
///
/// Autopack records each as a `configNote<N>` metadata entry whose text names
/// the skipped keys (e.g. "nixPkgs with no mise equivalent were skipped: ...").
/// Ordering is by `N`, not by map order, so the log reads the same way as
/// autopack's own output.
pub(crate) fn untranslated_config_notes<'a>(
    metadata: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    let mut notes: Vec<(u32, String)> = metadata
        .into_iter()
        .filter_map(|(key, value)| {
            let index = key.strip_prefix("configNote")?.parse::<u32>().ok()?;
            let value = value.trim();
            (!value.is_empty()).then(|| (index, value.to_string()))
        })
        .collect();
    notes.sort_by_key(|(index, _)| *index);
    notes
        .into_iter()
        .map(|(_, note)| format!("Build config setting not applied: {note}"))
        .collect()
}

/// Retain the pnpm-only helper for templates that use pnpm filters.
pub(crate) fn pnpm_app_directory(config: &DockerfileConfig<'_>) -> Result<Option<String>, String> {
    if !config.root_local_path.join("pnpm-workspace.yaml").is_file() {
        return Ok(None);
    }
    node_app_directory(config)
}

/// A selected workspace package confined to the explicit repository root.
pub(crate) fn node_app_directory(config: &DockerfileConfig<'_>) -> Result<Option<String>, String> {
    if config.root_local_path == config.local_path {
        return Ok(None);
    }
    let relative = config
        .local_path
        .strip_prefix(config.root_local_path)
        .map_err(|_| "Application directory escapes the workspace root".to_string())?;
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("Application directory escapes the workspace root".to_string());
    }
    if !super::pnpm_workspace::app_is_member(config.root_local_path, config.local_path)? {
        return Ok(None);
    }
    let text = relative
        .to_str()
        .ok_or_else(|| "Workspace application directory must be UTF-8".to_string())?;
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
    {
        return Err(format!(
            "Unsupported workspace application directory: {text}"
        ));
    }
    Ok(Some(text.to_string()))
}

/// The selected pip application when it needs a sibling package in the source
/// repository. Dependency paths may leave the app, never the repository.
pub fn python_app_directory(root: &Path, selected: &Path) -> Result<Option<String>, String> {
    if root == selected {
        return Ok(None);
    }
    let relative = selected
        .strip_prefix(root)
        .map_err(|_| "Python application directory escapes the source repository".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("Python application directory escapes the source repository".to_string());
    }
    let repository = App::new(root).map_err(|e| e.to_string())?;
    let requirements = relative.join("requirements.txt");
    if !repository.has_file(&requirements) {
        return Ok(None);
    }
    let metadata =
        std::fs::symlink_metadata(root.join(&requirements)).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Python requirements.txt must be a regular non-symlink file".to_string());
    }
    if metadata.len() > 1024 * 1024 {
        return Err("Python requirements.txt exceeds the 1 MiB planning limit".to_string());
    }
    let contents = repository
        .read_file(&requirements)
        .map_err(|e| e.to_string())?;
    let mut needs_repository = false;
    for line in contents.lines() {
        let requirement = line
            .trim()
            .strip_prefix("-e ")
            .or_else(|| line.trim().strip_prefix("--editable "))
            .unwrap_or(line.trim());
        let requirement = requirement.split(" #").next().unwrap_or(requirement).trim();
        if !requirement.starts_with("../") && !requirement.starts_with("./") {
            continue;
        }
        // Extras select optional dependencies; they are not part of the
        // local filesystem path. pip still receives the untouched requirement.
        let dependency_path = requirement
            .strip_suffix(']')
            .and_then(|with_extras| with_extras.rsplit_once('['))
            .map_or(requirement, |(path, _)| path);
        let mut resolved = std::path::PathBuf::new();
        for component in relative.join(dependency_path).components() {
            match component {
                std::path::Component::Normal(part) => resolved.push(part),
                std::path::Component::CurDir => {},
                std::path::Component::ParentDir if resolved.pop() => {},
                _ => return Err(format!("Local Python dependency '{requirement}' escapes the source repository. Include the package inside the uploaded repository, or use a Dockerfile with an explicit build context.")),
            }
        }
        if !repository.has_dir(&resolved) && !repository.has_file(&resolved) {
            return Err(format!("Local Python dependency '{requirement}' is missing or leaves the source repository through a symlink. Upload the complete repository or use a Dockerfile with an explicit repository build context."));
        }
        needs_repository |= !resolved.starts_with(relative);
    }
    if !needs_repository {
        return Ok(None);
    }
    let text = relative
        .to_str()
        .ok_or_else(|| "Python application directory must be UTF-8".to_string())?;
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
    {
        return Err("Unsupported Python application directory: use a Dockerfile with an explicit repository build context".to_string());
    }
    Ok(Some(text.to_string()))
}

/// True when `name` can be declared as a Dockerfile `ARG` by autopack.
fn is_build_arg_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Render, or fall back to a Dockerfile that fails loudly with the reason.
///
/// The trait cannot return an error, and returning an empty or plausible
/// Dockerfile would turn a detection failure into a confusing runtime failure
/// several minutes later — or, worse, an image that builds and then exits.
/// The failure is also recorded as [`BuildPlanFailure::Unplannable`] so the
/// deployment pipeline can refuse to build at all.
pub(crate) fn render_or_explain(
    config: &DockerfileConfig<'_>,
    provider: Option<&str>,
) -> DockerfileWithArgs {
    match render(config, provider) {
        Ok(dockerfile) => dockerfile,
        Err(message) => {
            warn!("autopack could not plan this application: {message}");
            // `failing` keeps the RUN instruction on one physical line even
            // when `message` spans several (e.g. a multi-line "how to fix
            // this" hint); the comment block keeps the original line breaks.
            DockerfileWithArgs::failing(BuildPlanFailure::Unplannable {
                preset: match provider {
                    Some(provider) => format!("autopack ({provider})"),
                    None => "autopack".to_string(),
                },
                reason: message.trim().to_string(),
            })
        }
    }
}

#[async_trait]
impl Preset for AutopackPreset {
    fn uses_autopack(&self) -> bool {
        true
    }

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

        assert!(
            result.content.starts_with("# syntax="),
            "{}",
            result.content
        );
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
    async fn a_multiline_failure_reason_stays_on_one_run_line() {
        // A detected-but-unstartable project (Python with dependencies but no
        // recognisable entrypoint, Procfile, or WSGI/ASGI module) hits
        // autopack_core::Error::MissingStartCommand, whose message spans two
        // lines. A raw newline spliced into `RUN echo '...'` used to split the
        // Dockerfile into a second physical line, which Docker parsed as its
        // own instruction — breaking on whatever word started that second
        // line (e.g. "unknown instruction: Set").
        let dir = fixture(&[
            ("requirements.txt", "flask==2.0.0\n"),
            ("README.md", "# no entrypoint here\n"),
        ]);

        let result = AutopackPreset::new()
            .dockerfile_with_build_dir(dir.path())
            .await;

        assert!(result.content.contains("exit 1"), "{}", result.content);
        let run_lines: Vec<&str> = result
            .content
            .lines()
            .filter(|l| l.trim_start().starts_with("RUN"))
            .collect();
        assert_eq!(run_lines.len(), 1, "{}", result.content);
        assert!(
            !result
                .content
                .lines()
                .any(|l| l.trim_start().starts_with("Set ")),
            "a bare 'Set' line means the RUN instruction got split by an embedded newline: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn platform_build_settings_reach_autopack() {
        let dir = node_app();
        let mut config = buildkit_config(dir.path());
        config.build_command = Some("npm run build:prod");

        let result = AutopackPreset::new().dockerfile(config).await;
        assert!(result.content.contains("build:prod"), "{}", result.content);
    }

    #[test]
    fn upgraded_autopack_preserves_next_and_pnpm_caches() {
        let dir = fixture(&[
            (
                "package.json",
                r#"{"packageManager":"pnpm@11.0.0","scripts":{"build":"next build","start":"next start"},"dependencies":{"next":"16.0.0"}}"#,
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("Next.js plan");
        assert!(
            result.content.contains("pnpm_config_store_dir"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("target=/cache/pnpm"),
            "{}",
            result.content
        );
        assert!(
            result
                .content
                .contains("target=/app/.next/cache,sharing=locked"),
            "{}",
            result.content
        );
    }

    #[test]
    fn upgraded_autopack_prepares_caddy_despite_user_step_overrides() {
        let dir = fixture(&[
            ("index.html", "<h1>static site</h1>"),
            (
                "autopack.json",
                r#"{"steps":{"caddy":{"commands":["echo custom step"]}}}"#,
            ),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("static plan");
        assert!(
            result
                .content
                .contains("RUN cp /usr/bin/caddy /tmp/caddy && mv /tmp/caddy /usr/bin/caddy"),
            "{}",
            result.content
        );
        assert!(
            result
                .content
                .contains("--from=autopack-caddy-1 /usr/bin/caddy /usr/bin/caddy"),
            "{}",
            result.content
        );
    }

    #[test]
    fn app_cache_scope_build_variables_reach_autopack() {
        let dir = node_app();
        let vars = vec![
            "AUTOPACK_CACHE_SCOPE=app".to_string(),
            "AUTOPACK_CACHE_KEY=project-a".to_string(),
        ];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("scoped plan");
        assert!(
            result.content.contains("id=autopack-70726f6a6563742d61-"),
            "{}",
            result.content
        );
        let missing_key = vec!["AUTOPACK_CACHE_SCOPE=app".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&missing_key);
        assert!(render(&config, None)
            .unwrap_err()
            .contains("AUTOPACK_CACHE_KEY"));
    }

    /// The lines of the stage declared `FROM ... AS {stage}`.
    fn stage<'a>(dockerfile: &'a str, stage: &str) -> Vec<&'a str> {
        let header = format!(" AS {stage}");
        dockerfile
            .lines()
            .skip_while(|line| !(line.starts_with("FROM ") && line.ends_with(&header)))
            .take_while(|line| !line.starts_with("# ----"))
            .collect()
    }

    #[test]
    fn project_variables_are_declared_in_the_build_step() {
        // `build_image.rs` passes names only; the values arrive as
        // `--build-arg`s, which a stage only sees for the `ARG`s it declares.
        let dir = fixture(&[
            (
                "package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let vars = vec!["VITE_API_URL".to_string(), "PUBLIC_SITE_NAME".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("Node plan");

        let build = stage(&result.content, "autopack-build");
        for name in ["VITE_API_URL", "PUBLIC_SITE_NAME"] {
            let declaration = format!("ARG {name}");
            assert!(build.contains(&declaration.as_str()), "{}", result.content);
            // Declared once, so a changed value never re-runs `npm ci`.
            assert_eq!(
                result.content.matches(&declaration).count(),
                1,
                "{}",
                result.content
            );
        }
    }

    #[test]
    fn variables_an_arg_cannot_declare_do_not_fail_the_build() {
        let dir = fixture(&[
            (
                "package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let vars = vec![
            "my-var".to_string(),
            "1ST".to_string(),
            "BAD\nRUN id".to_string(),
            "GOOD".to_string(),
        ];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("invalid names are skipped, not fatal");

        assert!(result.content.contains("ARG GOOD"), "{}", result.content);
        assert!(!result.content.contains("my-var"), "{}", result.content);
        assert!(!result.content.contains("ARG 1ST"), "{}", result.content);
        assert!(!result.content.contains("RUN id"), "{}", result.content);
    }

    /// The worker build guard in `temps_deployer::remote` refuses a Dockerfile
    /// in which any word is `ARG`, case-insensitively.
    fn worker_guard_finds_an_arg(dockerfile: &str) -> bool {
        dockerfile
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|word| word.eq_ignore_ascii_case("ARG"))
    }

    #[test]
    fn without_build_vars_the_dockerfile_still_builds_on_a_worker() {
        // Worker builds render without build variables, because a worker
        // never receives their values. That must leave no `ARG` behind, or
        // every Autopack build on a worker is refused.
        let dir = fixture(&[
            (
                "package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("Node plan");
        assert!(
            !worker_guard_finds_an_arg(&result.content),
            "{}",
            result.content
        );

        let vars = vec!["VITE_API_URL".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("Node plan");
        assert!(
            worker_guard_finds_an_arg(&result.content),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn presets_that_report_autopack_render_with_it() {
        // `uses_autopack` decides whether a worker build omits build
        // variables, so it has to match what the preset actually renders.
        let dir = fixture(&[
            (
                "package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let mut reported = Vec::new();
        for preset in crate::all_presets() {
            if !preset.uses_autopack() {
                continue;
            }
            reported.push(preset.slug());
            let result = preset.dockerfile(buildkit_config(dir.path())).await;
            // Either a generated plan or the fallback explaining why there is none.
            assert!(
                result.content.contains("Generated by autopack")
                    || result.content.contains("autopack could not plan"),
                "{} does not render with Autopack:\n{}",
                preset.slug(),
                result.content
            );
        }
        for slug in ["autopack", "nixpacks", "python", "go", "rust", "java"] {
            assert!(
                reported.iter().any(|s| s == slug),
                "{slug} must report Autopack"
            );
        }
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
        let dir = fixture(&[
            ("main.go", "package main\nfunc main() {}\n"),
            ("go.mod", "module x\n\ngo 1.22\n"),
        ]);
        let config = buildkit_config(dir.path());

        let forced = render(&config, Some("go")).expect("go provider");
        assert!(forced.content.contains("go build"), "{}", forced.content);
    }

    /// A .NET project file autopack detects anywhere in the tree (`**/*.csproj`),
    /// so it changes the outcome if the scan ever reaches it through a link.
    const CSPROJ: &str = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>"#;

    /// Render on a worker thread so a scan that follows a symlink loop fails
    /// the test instead of hanging the suite.
    fn render_with_timeout(root: std::path::PathBuf) -> Result<DockerfileWithArgs, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(render(&buildkit_config(&root), None));
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("render did not finish within 10s: the scan followed a symlink loop")
    }

    #[cfg(unix)]
    #[test]
    fn the_build_scan_does_not_follow_symlinks() {
        // Control: the same project file in a real directory is detected, so
        // the assertion below fails if the scan starts following links.
        let real = fixture(&[("README.md", "# docs"), ("real/App.csproj", CSPROJ)]);
        assert!(
            render_with_timeout(real.path().to_path_buf()).is_ok(),
            "a real .csproj must be detected for this test to mean anything"
        );

        let root = fixture(&[("README.md", "# docs")]);
        let outside = fixture(&[("App.csproj", CSPROJ)]);
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked_dir")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("App.csproj"),
            root.path().join("Linked.csproj"),
        )
        .unwrap();
        std::os::unix::fs::symlink(".", root.path().join("a")).unwrap();
        std::os::unix::fs::symlink(".", root.path().join("b")).unwrap();

        let result = render_with_timeout(root.path().to_path_buf());

        assert!(
            result.is_err(),
            "a project reachable only through symlinks must not be planned: {:?}",
            result.map(|dockerfile| dockerfile.content)
        );
    }
    #[test]
    fn nested_go_module_with_a_sibling_replace_builds_in_its_directory() {
        let repo = fixture(&[
            ("go.work", "go 1.22\n\nuse (\n\t./apps/api\n\t./packages/shared\n)\n"),
            (
                "apps/api/go.mod",
                "module example.test/qa/api\n\ngo 1.22\n\nrequire example.test/qa/shared v0.0.0\n\nreplace example.test/qa/shared => ../../packages/shared\n",
            ),
            (
                "apps/api/main.go",
                "package main\n\nimport \"net/http\"\n\nfunc main() { http.ListenAndServe(\":8080\", nil) }\n",
            ),
            ("packages/shared/go.mod", "module example.test/qa/shared\n\ngo 1.22\n"),
            ("packages/shared/shared.go", "package shared\n"),
        ]);
        let app = repo.path().join("apps/api");
        let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
        config.use_buildkit = true;
        let nested = render(&config, Some("go")).unwrap().content;
        assert!(nested.contains("cd /app/apps/api && "), "{nested}");
        assert!(!nested.contains("GOWORK=off"), "{nested}");

        // Built alone (the default), nothing changes.
        let alone = render(&buildkit_config(&app), Some("go")).unwrap().content;
        assert!(!alone.contains("cd /app/apps/api"), "{alone}");
    }

    #[test]
    fn nested_cargo_crate_with_a_sibling_path_dependency_builds_in_its_directory() {
        let repo = fixture(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/api\", \"packages/shared\"]\nresolver = \"2\"\n",
            ),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"qa-api\"\npath = \"src/main.rs\"\n\n[dependencies]\nqa-shared = { path = \"../../packages/shared\" }\n",
            ),
            ("apps/api/src/main.rs", "fn main() {}\n"),
            (
                "packages/shared/Cargo.toml",
                "[package]\nname = \"qa-shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("packages/shared/src/lib.rs", ""),
        ]);
        let app = repo.path().join("apps/api");
        let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
        config.use_buildkit = true;
        let nested = render(&config, Some("rust")).unwrap().content;
        assert!(nested.contains("cd /app/apps/api && "), "{nested}");
        assert!(nested.contains("CARGO_TARGET_DIR=/app/target"), "{nested}");
    }

    #[test]
    fn nested_pnpm_server_uses_root_version_lock_and_selected_entrypoint() {
        let repo = fixture(&[
            (
                "package.json",
                r#"{"private":true,"packageManager":"pnpm@10.15.1","scripts":{"start":"node wrong.js","build":"node wrong.js"}}"#,
            ),
            ("pnpm-workspace.yaml", "packages: [apps/*, packages/*]"),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'"),
            (
                "apps/api/package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"},"dependencies":{"@fixture/shared":"workspace:*"}}"#,
            ),
            ("apps/api/server.js", ""),
            (
                "packages/shared/package.json",
                r#"{"name":"@fixture/shared"}"#,
            ),
        ]);
        let app = repo.path().join("apps/api");
        let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
        config.use_buildkit = true;
        let result = render(&config, Some("node")).unwrap().content;
        assert!(result.contains("10.15.1"), "{result}");
        assert!(result.contains("--frozen-lockfile"), "{result}");
        assert!(
            result.contains("pnpm --filter './apps/api...' --if-present run build"),
            "{result}"
        );
        assert!(
            result.contains(
                "cd /app/apps/api && PATH=/app/apps/api/node_modules/.bin:$PATH node server.js"
            ),
            "{result}"
        );
        assert!(!result.contains("node wrong.js"), "{result}");
    }

    #[test]
    fn python_sibling_packages_keep_install_build_and_start_cwd() {
        let repo = fixture(&[
            (
                "apps/api/requirements.txt",
                "../../packages/shared\nflask==3.1.2",
            ),
            (
                "apps/api/app.py",
                "from flask import Flask\napp = Flask(__name__)",
            ),
            (
                "packages/shared/pyproject.toml",
                "[project]\nname='fixture-shared'\nversion='1.0.0'",
            ),
        ]);
        let app = repo.path().join("apps/api");
        let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
        assert_eq!(
            python_app_directory(repo.path(), &app).unwrap(),
            Some("apps/api".to_string())
        );
        let rendered = render(&config, Some("python")).unwrap().content;
        assert!(rendered.contains("COPY . /app"), "{rendered}");
        assert!(
            rendered.contains("cd /app/apps/api && sh -c 'pip install -r requirements.txt'"),
            "{rendered}"
        );
        assert!(
            rendered.contains("cd /app/apps/api && gunicorn app:app"),
            "{rendered}"
        );
    }

    /// An absolute launcher does not make the module it serves absolute: a
    /// nested Python app's `Procfile` start still runs in the app directory.
    #[test]
    fn nested_python_absolute_start_command_keeps_the_app_directory() {
        let repo = fixture(&[
            (
                "apps/api/requirements.txt",
                "../../packages/shared\nflask==3.1.2\ngunicorn==23.0.0",
            ),
            (
                "apps/api/app.py",
                "from flask import Flask\napp = Flask(__name__)",
            ),
            ("apps/api/Procfile", "web: /usr/bin/env gunicorn app:app"),
            (
                "packages/shared/pyproject.toml",
                "[project]\nname='fixture-shared'\nversion='1.0.0'",
            ),
        ]);
        let app = repo.path().join("apps/api");
        let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
        let rendered = render(&config, Some("python")).unwrap().content;
        assert!(
            rendered.contains("cd /app/apps/api && /usr/bin/env gunicorn app:app"),
            "{rendered}"
        );
    }

    #[test]
    fn python_dependency_paths_cannot_escape_the_supplied_repository() {
        let repo = fixture(&[
            ("apps/api/requirements.txt", "../../../outside"),
            ("apps/api/app.py", ""),
        ]);
        let app = repo.path().join("apps/api");
        let error = python_app_directory(repo.path(), &app).unwrap_err();
        assert!(error.contains("escapes the source repository"), "{error}");
        assert!(error.contains("Dockerfile"), "{error}");
        std::fs::write(app.join("requirements.txt"), "../../packages/missing").unwrap();
        assert!(python_app_directory(repo.path(), &app)
            .unwrap_err()
            .contains("missing"));
        std::fs::write(app.join("requirements.txt"), "flask==3.1.2").unwrap();
        assert_eq!(python_app_directory(repo.path(), &app).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn python_local_dependency_symlinks_never_import_host_files() {
        let outside = tempfile::tempdir().unwrap();
        let repo = fixture(&[("apps/api/requirements.txt", "../../packages/shared")]);
        std::fs::create_dir_all(repo.path().join("packages")).unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("packages/shared")).unwrap();
        assert!(
            python_app_directory(repo.path(), &repo.path().join("apps/api"))
                .unwrap_err()
                .contains("symlink")
        );
    }

    #[test]
    fn standalone_app_paths_do_not_receive_python_shell_restrictions() {
        for directory in ["apps/my api", "apps/café"] {
            let requirement = format!("{directory}/requirements.txt");
            let local = format!("{directory}/vendor/shared/pyproject.toml");
            let repo = fixture(&[
                (&requirement, "flask==3.1.2"),
                (&local, "[project]\nname='shared'\nversion='1.0.0'"),
            ]);
            let app = repo.path().join(directory);
            assert_eq!(python_app_directory(repo.path(), &app).unwrap(), None);
            std::fs::write(app.join("requirements.txt"), "./vendor/shared").unwrap();
            assert_eq!(python_app_directory(repo.path(), &app).unwrap(), None);
            std::fs::remove_file(app.join("requirements.txt")).unwrap();
            assert_eq!(python_app_directory(repo.path(), &app).unwrap(), None);
            std::fs::write(
                app.join("package.json"),
                r#"{"scripts":{"start":"node server.js"}}"#,
            )
            .unwrap();
            std::fs::write(app.join("server.js"), "").unwrap();
            let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
            assert!(render(&config, Some("node")).is_ok());
        }
    }

    #[test]
    fn node_workspace_requirements_do_not_force_python_context_validation() {
        let repo = fixture(&[
            (
                "package.json",
                r#"{"private":true,"workspaces":["apps/*"]}"#,
            ),
            (
                "apps/api/package.json",
                r#"{"scripts":{"start":"node server.js"}}"#,
            ),
            ("apps/api/server.js", ""),
            ("apps/api/requirements.txt", "../../../outside"),
        ]);
        let app = repo.path().join("apps/api");
        let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
        assert!(render(&config, None).is_ok());
    }

    #[test]
    fn python_local_package_extras_preserve_the_dependency_path_and_context() {
        let repo = fixture(&[
            ("apps/api/requirements.txt", "./vendor/shared[http]"),
            (
                "apps/api/vendor/shared/pyproject.toml",
                "[project]\nname='local'\nversion='1.0.0'",
            ),
            (
                "packages/shared/pyproject.toml",
                "[project]\nname='sibling'\nversion='1.0.0'",
            ),
        ]);
        let app = repo.path().join("apps/api");
        assert_eq!(python_app_directory(repo.path(), &app).unwrap(), None);
        for requirement in [
            "../../packages/shared[http,cli]",
            "-e ../../packages/shared[http]",
        ] {
            std::fs::write(app.join("requirements.txt"), requirement).unwrap();
            assert_eq!(
                python_app_directory(repo.path(), &app).unwrap(),
                Some("apps/api".to_string())
            );
            assert_eq!(
                std::fs::read_to_string(app.join("requirements.txt")).unwrap(),
                requirement
            );
        }
        std::fs::write(app.join("requirements.txt"), "../../../outside[http]").unwrap();
        assert!(python_app_directory(repo.path(), &app)
            .unwrap_err()
            .contains("escapes"));
    }

    #[test]
    fn npm_yarn_and_bun_use_root_manager_and_selected_app() {
        for (pin, manager, install) in [
            ("npm@10.9.0", "npm", "npm install"),
            ("yarn@4.6.0", "yarn", "yarn install"),
            ("bun@1.2.22", "bun", "bun install"),
        ] {
            let root = serde_json::json!({"private":true,"packageManager":pin,"workspaces":["apps/*","packages/*"],"scripts":{"start":"node wrong.js","build":"node wrong.js"}}).to_string();
            let repo = fixture(&[
                ("package.json", &root),
                (
                    "apps/api/package.json",
                    r#"{"scripts":{"start":"node server.js","build":"node build.js"},"dependencies":{"@fixture/shared":"workspace:*"}}"#,
                ),
                ("apps/api/server.js", ""),
                (
                    "packages/shared/package.json",
                    r#"{"name":"@fixture/shared","version":"1.0.0"}"#,
                ),
            ]);
            let app = repo.path().join("apps/api");
            let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
            let result = render(&config, Some("node")).unwrap().content;
            assert!(result.contains(install), "{pin}: {result}");
            assert!(
                result.contains(&format!("cd /app/apps/api && {manager} run build")),
                "{pin}: {result}"
            );
            let start = if manager == "yarn" {
                "yarn run start"
            } else {
                "node server.js"
            };
            assert!(
                result.contains(&format!(
                    "PATH=/app/apps/api/node_modules/.bin:$PATH {start}"
                )),
                "{pin}: {result}"
            );
            assert!(!result.contains("node wrong.js"), "{pin}: {result}");
            assert!(result.contains("COPY . /app"), "{pin}: {result}");
        }
    }

    #[test]
    fn workspace_manager_selection_preserves_explicit_app_start_commands() {
        for pin in ["npm@10.9.0", "yarn@4.6.0", "bun@1.2.22"] {
            let root =
                serde_json::json!({"private":true,"packageManager":pin,"workspaces":["apps/*"]})
                    .to_string();
            for (file, contents) in [
                ("Procfile", "web: node configured.js"),
                (
                    "autopack.json",
                    r#"{"deploy":{"startCommand":"node configured.js"}}"#,
                ),
            ] {
                let config_path = format!("apps/api/{file}");
                let repo = fixture(&[
                    ("package.json", &root),
                    (
                        "apps/api/package.json",
                        r#"{"scripts":{"start":"node wrong.js"}}"#,
                    ),
                    (&config_path, contents),
                    ("apps/api/configured.js", ""),
                ]);
                let app = repo.path().join("apps/api");
                let config =
                    DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
                let result = render(&config, Some("node")).unwrap().content;
                assert!(
                    result
                        .contains("PATH=/app/apps/api/node_modules/.bin:$PATH node configured.js"),
                    "{pin}: {result}"
                );
                assert!(!result.contains("run start"), "{pin}: {result}");
                assert!(!result.contains("node wrong.js"), "{pin}: {result}");
            }
        }
    }

    #[test]
    fn explicit_workspace_manager_commands_are_not_rewritten() {
        for pin in ["yarn@4.6.0", "bun@1.2.22"] {
            let root =
                serde_json::json!({"private":true,"packageManager":pin,"workspaces":["apps/*"]})
                    .to_string();
            for (file, contents) in [
                ("Procfile", "web: npm run start"),
                (
                    "autopack.json",
                    r#"{"deploy":{"startCommand":"npm run start"}}"#,
                ),
                ("nixpacks.toml", "[start]\ncmd = 'npm run start'"),
                ("", ""),
            ] {
                let config_path = format!("apps/api/{file}");
                let mut files: Vec<(&str, &str)> = vec![
                    ("package.json", &root),
                    (
                        "apps/api/package.json",
                        r#"{"scripts":{"start":"node server.js"}}"#,
                    ),
                    ("apps/api/server.js", ""),
                ];
                if !file.is_empty() {
                    files.push((&config_path, contents));
                }
                let repo = fixture(&files);
                let app = repo.path().join("apps/api");
                let vars = vec!["AUTOPACK_START_CMD=npm run start".to_string()];
                let mut config =
                    DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
                if file.is_empty() {
                    config = config.with_build_vars(&vars);
                }
                let result = render(&config, Some("node")).unwrap().content;
                assert!(
                    result.contains("PATH=/app/apps/api/node_modules/.bin:$PATH npm run start"),
                    "{pin} {file}: {result}"
                );
            }
        }
    }

    #[test]
    fn compound_workspace_start_scripts_use_the_root_manager() {
        for (pin, manager) in [
            ("npm@10.9.0", "npm"),
            ("yarn@4.6.0", "yarn"),
            ("bun@1.2.22", "bun"),
        ] {
            let root =
                serde_json::json!({"private":true,"packageManager":pin,"workspaces":["apps/*"]})
                    .to_string();
            let repo = fixture(&[
                ("package.json", &root),
                (
                    "apps/api/package.json",
                    r#"{"scripts":{"start":"node prepare.js && node server.js"}}"#,
                ),
                ("apps/api/server.js", ""),
            ]);
            let app = repo.path().join("apps/api");
            let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
            let result = render(&config, Some("node")).unwrap().content;
            assert!(
                result.contains(&format!(
                    "PATH=/app/apps/api/node_modules/.bin:$PATH {manager} run start"
                )),
                "{pin}: {result}"
            );
        }
    }

    #[test]
    fn extglob_node_members_use_workspace_install_and_selected_entrypoint() {
        let repo = fixture(&[
            (
                "pnpm-workspace.yaml",
                "packages: ['apps/@(web|api|private)', '!apps/@(private|internal)']",
            ),
            (
                "package.json",
                r#"{"name":"root","packageManager":"pnpm@9.15.9"}"#,
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'"),
        ]);
        for (name, member) in [
            ("web", true),
            ("api", true),
            ("private", false),
            ("mobile", false),
        ] {
            let app = repo.path().join("apps").join(name);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(
                app.join("package.json"),
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            )
            .unwrap();
            let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
            let result = render(&config, Some("node")).unwrap().content;
            assert_eq!(result.contains("--filter"), member, "{name}: {result}");
            assert_eq!(
                result.contains(&format!("cd /app/apps/{name}")),
                member,
                "{name}: {result}"
            );
        }
    }

    #[test]
    fn nonmember_node_apps_are_analyzed_locally_without_workspace_filters() {
        let repo = fixture(&[
            (
                "pnpm-workspace.yaml",
                "packages: ['apps/*', '!apps/private']",
            ),
            (
                "package.json",
                r#"{"name":"root","packageManager":"pnpm@9.15.9"}"#,
            ),
            (
                "apps/private/package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
            (
                "tools/web/package.json",
                r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#,
            ),
        ]);
        for relative in ["apps/private", "tools/web"] {
            let app = repo.path().join(relative);
            let config = DockerfileConfig::new(repo.path(), &app, "fixture").with_buildkit(true);
            assert_eq!(pnpm_app_directory(&config).unwrap(), None);
            let dockerfile = render(&config, Some("node")).unwrap().content;
            assert!(!dockerfile.contains("--filter"), "{dockerfile}");
            assert!(
                !dockerfile.contains(&format!("/app/{relative}")),
                "{dockerfile}"
            );
        }
    }

    #[test]
    fn workspace_directory_rejects_escape_and_shell_metacharacters() {
        let repo = fixture(&[
            ("pnpm-workspace.yaml", "packages: [apps/*]"),
            ("apps/web;echo/package.json", "{}"),
        ]);
        for path in [
            repo.path().join("../other"),
            repo.path().join("apps/web;echo"),
        ] {
            let config = DockerfileConfig::new(repo.path(), &path, "fixture");
            assert!(pnpm_app_directory(&config).is_err());
        }
    }

    #[test]
    fn adopted_ruby_provider_preserves_version_and_online_locked_install() {
        for version in ["3.3.6", "ruby-3.3.6", "3.4.11", "ruby-3.4.11"] {
            let repo = fixture(&[
                ("Gemfile", "source 'https://rubygems.org'\ngem 'rack'\n"),
                ("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.8)\n\nPLATFORMS\n  ruby\n  x86_64-linux\n\nDEPENDENCIES\n  rack\n\nBUNDLED WITH\n   2.6.9\n"),
                (".ruby-version", version),
                ("config.ru", "run ->(_env) { [200, {}, ['ok']] }\n"),
            ]);
            let result = render(&buildkit_config(repo.path()), Some("ruby"))
                .unwrap()
                .content;
            let expected = if version.contains("3.4") {
                "ruby:3.4"
            } else {
                "ruby:3.3"
            };
            assert!(result.contains(expected), "{result}");
            assert!(result.contains("BUNDLE_DEPLOYMENT=1"), "{result}");
            assert!(result.contains("BUNDLE_USER_CACHE"), "{result}");
            assert!(!result.contains("BUNDLE_CACHE_PATH"), "{result}");
        }
    }

    #[test]
    fn adopted_php_provider_removes_file_capability_and_inherited_admin_probe() {
        let repo = fixture(&[
            ("composer.json", r#"{"require":{"php":"^8.4"}}"#),
            ("index.php", "<?php echo 'ok';"),
        ]);
        let result = render(&buildkit_config(repo.path()), Some("php"))
            .unwrap()
            .content;
        assert!(
            result.contains("cp /usr/local/bin/frankenphp /usr/local/bin/frankenphp.autopack"),
            "{result}"
        );
        assert!(result.contains("HEALTHCHECK NONE"), "{result}");
        assert!(result.contains("admin off"), "{result}");
        assert!(result.contains("10001"), "{result}");
    }

    #[test]
    fn malformed_ruby_pin_fails_with_requested_version_instead_of_defaulting() {
        for pin in ["ruby-bad", "3..4", "jruby-9.4.0.0"] {
            let repo = fixture(&[
                ("Gemfile", "source 'https://rubygems.org'\n"),
                (".ruby-version", pin),
            ]);
            let error = render(&buildkit_config(repo.path()), Some("ruby")).unwrap_err();
            assert!(error.contains(pin), "{error}");
            assert!(error.contains("refusing to silently choose"));
        }
    }

    #[test]
    fn ruby_gemfile_and_lockfile_version_conventions_are_preserved() {
        for files in [
            vec![("Gemfile", "source 'https://rubygems.org'\nruby '3.4.11'\n"), ("Procfile", "web: ruby server.rb"), ("server.rb", "")],
            vec![("Gemfile", "source 'https://rubygems.org'\n"), ("Gemfile.lock", "GEM\n  specs:\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n\nRUBY VERSION\n   ruby 3.4.11p0\n\nBUNDLED WITH\n   2.6.9\n"), ("Procfile", "web: ruby server.rb"), ("server.rb", "")],
        ] {
            let repo = fixture(&files);
            let result = render(&buildkit_config(repo.path()), Some("ruby")).unwrap().content;
            assert!(result.contains("ruby:3.4"), "{result}");
        }
    }

    #[tokio::test]
    async fn an_unplannable_app_reports_a_typed_plan_failure() {
        let dir = fixture(&[("notes.txt", "nothing to build here")]);

        let result = AutopackPreset::new()
            .dockerfile_with_build_dir(dir.path())
            .await;

        let Some(BuildPlanFailure::Unplannable { preset, reason }) = result.plan_failure else {
            panic!(
                "expected an Unplannable plan failure, got {:?}",
                result.plan_failure
            );
        };
        assert_eq!(preset, "autopack");
        assert!(reason.contains("no provider could be detected"), "{reason}");
    }

    #[tokio::test]
    async fn a_plannable_app_reports_no_plan_failure() {
        let dir = node_app();
        let result = AutopackPreset::new()
            .dockerfile_with_build_dir(dir.path())
            .await;
        assert!(result.plan_failure.is_none(), "{:?}", result.plan_failure);
    }

    #[test]
    fn untranslated_config_notes_are_ordered_by_index_and_ignore_other_metadata() {
        let notes = untranslated_config_notes([
            ("provider", "node"),
            ("configNote10", "tenth"),
            (
                "configNote2",
                "phases other than setup/install/build were skipped: release",
            ),
            (
                "configNote1",
                "nixPkgs with no mise equivalent were skipped: ffmpeg",
            ),
            ("configNoteX", "not a numbered note"),
            ("configNote3", "   "),
        ]);
        assert_eq!(
            notes,
            vec![
                "Build config setting not applied: nixPkgs with no mise equivalent were skipped: ffmpeg",
                "Build config setting not applied: phases other than setup/install/build were skipped: release",
                "Build config setting not applied: tenth",
            ]
        );
        assert!(untranslated_config_notes([("provider", "node")]).is_empty());
    }

    #[test]
    fn legacy_nixpacks_keys_that_cannot_be_translated_surface_as_warnings() {
        let dir = fixture(&[
            ("package.json", r#"{"scripts":{"start":"node server.js"}}"#),
            ("server.js", ""),
            (
                "nixpacks.toml",
                "[phases.setup]\nnixPkgs = ['definitely-not-a-mise-tool']\n\n[phases.release]\ncmds = ['echo release']\n",
            ),
        ]);
        let result = render(&buildkit_config(dir.path()), None).unwrap();
        let joined = result.warnings.join("\n");
        assert!(joined.contains("definitely-not-a-mise-tool"), "{joined}");
        assert!(joined.contains("release"), "{joined}");
    }
}
