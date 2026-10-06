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

    let workspace_app = if provider.is_none() || provider == Some("node") {
        pnpm_app_directory(config)?
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
        let start = selected_analysis
            .plan
            .deploy
            .start_command
            .as_deref()
            .ok_or_else(|| {
                format!("No start command found for workspace application {relative}")
            })?;
        env.set("AUTOPACK_PROVIDER", "node");
        env.set(
            "AUTOPACK_START_CMD",
            format!("cd /app/{relative} && PATH=/app/{relative}/node_modules/.bin:$PATH {start}"),
        );
        if config.build_command.is_none() {
            env.set(
                "AUTOPACK_BUILD_CMD",
                format!("pnpm --filter './{relative}...' --if-present run build"),
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
    let analysis = analyze(&app, &env, &registry).map_err(|e| e.to_string())?;

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

/// A pnpm workspace app directory safe to use in Dockerfile paths and shell filters.
/// The build job confines the root and checks the workspace marker before calling us.
pub(crate) fn pnpm_app_directory(config: &DockerfileConfig<'_>) -> Result<Option<String>, String> {
    if config.root_local_path == config.local_path
        || !config.root_local_path.join("pnpm-workspace.yaml").is_file()
    {
        return Ok(None);
    }
    let relative = config
        .local_path
        .strip_prefix(config.root_local_path)
        .map_err(|_| "Application directory escapes the pnpm workspace root".to_string())?;
    let text = relative
        .to_str()
        .ok_or_else(|| "pnpm workspace application directory must be UTF-8".to_string())?;
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "Unsupported pnpm workspace application directory: {text}"
        ));
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
pub(crate) fn render_or_explain(
    config: &DockerfileConfig<'_>,
    provider: Option<&str>,
) -> DockerfileWithArgs {
    match render(config, provider) {
        Ok(dockerfile) => dockerfile,
        Err(message) => {
            warn!("autopack could not plan this application: {message}");
            // `message` can itself contain newlines (e.g. a multi-line "how to
            // fix this" hint). A raw newline inside the `RUN echo '...'`
            // argument would split it into a second physical Dockerfile line
            // that Docker parses as its own instruction — collapse to spaces
            // so the RUN instruction stays on one line; the comment block
            // above still renders the message with its original line breaks.
            let single_line_message = message.split_whitespace().collect::<Vec<_>>().join(" ");
            DockerfileWithArgs::new(format!(
                "# autopack could not plan this application.\n\
                 #\n\
                 # {}\n\
                 FROM debian:bookworm-slim\n\
                 RUN echo {} >&2 && exit 1\n",
                message.replace('\n', "\n# "),
                shell_quote(&single_line_message)
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
            ("package.json", r#"{"packageManager":"pnpm@11.0.0","scripts":{"build":"next build","start":"next start"},"dependencies":{"next":"16.0.0"}}"#),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("Next.js plan");
        assert!(result.content.contains("pnpm_config_store_dir"), "{}", result.content);
        assert!(result.content.contains("target=/cache/pnpm"), "{}", result.content);
        assert!(result.content.contains("target=/app/.next/cache,sharing=locked"), "{}", result.content);
    }

    #[test]
    fn upgraded_autopack_prepares_caddy_despite_user_step_overrides() {
        let dir = fixture(&[
            ("index.html", "<h1>static site</h1>"),
            ("autopack.json", r#"{"steps":{"caddy":{"commands":["echo custom step"]}}}"#),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("static plan");
        assert!(result.content.contains("RUN cp /usr/bin/caddy /tmp/caddy && mv /tmp/caddy /usr/bin/caddy"), "{}", result.content);
        assert!(result.content.contains("--from=autopack-caddy-1 /usr/bin/caddy /usr/bin/caddy"), "{}", result.content);
    }

    #[test]
    fn app_cache_scope_build_variables_reach_autopack() {
        let dir = node_app();
        let vars = vec!["AUTOPACK_CACHE_SCOPE=app".to_string(), "AUTOPACK_CACHE_KEY=project-a".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("scoped plan");
        assert!(result.content.contains("id=autopack-70726f6a6563742d61-"), "{}", result.content);
        let missing_key = vec!["AUTOPACK_CACHE_SCOPE=app".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&missing_key);
        assert!(render(&config, None).unwrap_err().contains("AUTOPACK_CACHE_KEY"));
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
            ("package.json", r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#),
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
            assert_eq!(result.content.matches(&declaration).count(), 1, "{}", result.content);
        }
    }

    #[test]
    fn variables_an_arg_cannot_declare_do_not_fail_the_build() {
        let dir = fixture(&[
            ("package.json", r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#),
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
            ("package.json", r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#),
            ("package-lock.json", "{}"),
        ]);
        let result = render(&buildkit_config(dir.path()), None).expect("Node plan");
        assert!(!worker_guard_finds_an_arg(&result.content), "{}", result.content);

        let vars = vec!["VITE_API_URL".to_string()];
        let config = buildkit_config(dir.path()).with_build_vars(&vars);
        let result = render(&config, None).expect("Node plan");
        assert!(worker_guard_finds_an_arg(&result.content), "{}", result.content);
    }

    #[tokio::test]
    async fn presets_that_report_autopack_render_with_it() {
        // `uses_autopack` decides whether a worker build omits build
        // variables, so it has to match what the preset actually renders.
        let dir = fixture(&[
            ("package.json", r#"{"scripts":{"build":"node build.js","start":"node server.js"}}"#),
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
            assert!(reported.iter().any(|s| s == slug), "{slug} must report Autopack");
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
        let dir = fixture(&[("main.go", "package main\nfunc main() {}\n"), ("go.mod", "module x\n\ngo 1.22\n")]);
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
    fn workspace_directory_rejects_escape_and_shell_metacharacters() {
        let repo = fixture(&[("pnpm-workspace.yaml", "packages: [apps/*]")]);
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
}
