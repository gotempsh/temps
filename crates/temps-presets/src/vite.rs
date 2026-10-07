// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{BuildPlanFailure, DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use std::path::Path;

pub struct Vite;

#[async_trait]
impl Preset for Vite {
    fn slug(&self) -> String {
        "vite".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "Vite".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/vite.svg".to_string()
    }

    async fn dockerfile(&self, config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        match super::autopack_preset::pnpm_app_directory(&config) {
            Ok(Some(relative)) => return workspace_dockerfile(&config, &relative),
            Ok(None) => {}
            Err(message) => {
                return DockerfileWithArgs::failing(BuildPlanFailure::InvalidConfiguration {
                    preset: "vite".to_string(),
                    reason: format!("invalid pnpm workspace configuration: {message}"),
                });
            }
        }
        // Without a build script `npm run build` stops with "Missing script:
        // build" only after every dependency has been installed. Say so now.
        if config.build_command.is_none() {
            if let Some(failure) = missing_build_script(config.local_path) {
                return DockerfileWithArgs::failing(failure);
            }
        }
        let package_manager = PackageManager::detect(config.local_path);
        let toolchain = JsToolchain::detect(config.local_path, package_manager);
        let install_cmd = config
            .install_command
            .unwrap_or(toolchain.install_command());
        let build_cmd = config
            .build_command
            .unwrap_or(package_manager.build_command());
        let mut warnings = Vec::new();
        let output = resolve_output_dir(config.output_dir, config.local_path, &mut warnings);

        // Use multi-stage build without BuildKit-specific --mount syntax.
        // Install-time configuration is copied before the install step: the
        // registry/auth settings in `.npmrc`, Yarn Berry's `.yarnrc.yml` (and
        // the release/plugins it points at), and patch directories applied
        // during install. The `*` suffix tolerates files that are absent or
        // excluded by `.dockerignore`.
        let mut dockerfile = format!(
            r#"FROM {} as builder
WORKDIR /app

# Copy package files and install-time configuration
COPY package.json package-lock.json* yarn.lock* pnpm-lock.yaml* bun.lock* .npmrc* .yarnrc* ./
{}{}{}
# Install dependencies
RUN {}{}

# Copy source code
COPY . .
"#,
            package_manager.base_image(),
            package_manager.dependency_config_copy(config.local_path),
            install_directory_copies(config.local_path),
            if toolchain.corepack {
                "ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0\n"
            } else {
                ""
            },
            if toolchain.corepack {
                "corepack enable && "
            } else {
                ""
            },
            install_cmd
        );

        // Add build variables if present
        if let Some(vars) = config.build_vars {
            for var in vars {
                dockerfile.push_str(&format!("ARG {}\n", var));
            }
        }

        dockerfile.push_str(&format!(
            r#"
# Build application
RUN {}

# Production stage with nginx
FROM nginx:alpine
COPY --from=builder /app/{} /usr/share/nginx/html
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
"#,
            build_cmd, output
        ));

        let mut rendered = DockerfileWithArgs::new(dockerfile);
        rendered.warnings = warnings;
        rendered
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        let pkg_manager = PackageManager::detect(local_path);

        let content = format!(
            r#"
FROM {}

WORKDIR /app

# Copy only the dist directory
COPY dist ./dist

# Install serve
RUN {}

# Expose the port the app runs on
EXPOSE 3000

CMD ["serve", "-s", "dist", "-l", "3000"]
"#,
            pkg_manager.base_image(),
            match pkg_manager {
                PackageManager::Bun => "bun install -g serve",
                PackageManager::Yarn => "yarn global add serve",
                PackageManager::Npm => "npm install -g serve",
                PackageManager::Pnpm => "npm install -g serve",
            }
        );
        DockerfileWithArgs::new(content)
    }

    fn install_command(&self, local_path: &Path) -> String {
        PackageManager::detect(local_path)
            .install_command()
            .to_string()
    }

    fn build_command(&self, local_path: &Path) -> String {
        PackageManager::detect(local_path)
            .build_command()
            .to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec!["/usr/share/nginx/html".to_string()]
    }
    fn default_port(&self) -> u16 {
        5173 // Vite dev server default port
    }

    fn static_output_dir(&self) -> Option<String> {
        Some("/usr/share/nginx/html".to_string())
    }
}

impl std::fmt::Display for Vite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

fn workspace_dockerfile(
    config: &super::DockerfileConfig<'_>,
    relative: &str,
) -> DockerfileWithArgs {
    let install = config
        .install_command
        .unwrap_or("pnpm install --frozen-lockfile");
    let build = config.build_command.map(str::to_owned).unwrap_or_else(|| {
        format!("cd /app && pnpm --filter './{relative}...' --if-present run build")
    });
    let mut dockerfile = format!("FROM node:22 AS builder\nWORKDIR /app\nRUN corepack enable\nCOPY . .\nENV CI=true\nRUN {install}\nWORKDIR /app/{relative}\n");
    for variable in config.build_vars.into_iter().flatten() {
        dockerfile.push_str(&format!("ARG {variable}\n"));
    }
    let mut warnings = Vec::new();
    let output = resolve_output_dir(config.output_dir, config.local_path, &mut warnings);
    dockerfile.push_str(&format!("RUN {build}\nFROM nginx:alpine\nCOPY --from=builder /app/{relative}/{output} /usr/share/nginx/html\nEXPOSE 80\nCMD [\"nginx\", \"-g\", \"daemon off;\"]\n"));
    let mut rendered = DockerfileWithArgs::new(dockerfile);
    rendered.warnings = warnings;
    rendered
}

/// Upper bound on the manifests and config files read while planning. A
/// legitimate `package.json` or `vite.config.*` is a few kilobytes.
const MAX_PLANNING_FILE_BYTES: u64 = 1024 * 1024;

/// Read a small planning input from the build context. Symlinks, directories
/// and oversized files are treated as absent: planning must never follow a
/// repository symlink out of the checkout.
fn read_planning_file(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PLANNING_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// A [`BuildPlanFailure::MissingBuildScript`] when `package.json` parses and
/// declares no `build` script. An absent or unparseable manifest is left to
/// the build itself, which reports the precise problem.
fn missing_build_script(local_path: &Path) -> Option<BuildPlanFailure> {
    let manifest_path = local_path.join("package.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&read_planning_file(&manifest_path)?).ok()?;
    let has_build = manifest
        .get("scripts")
        .and_then(|scripts| scripts.get("build"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|script| !script.trim().is_empty());
    (!has_build).then(|| BuildPlanFailure::MissingBuildScript {
        preset: "vite".to_string(),
        package_json: manifest_path.display().to_string(),
    })
}

/// How the dependency install step must provision and invoke the package
/// manager.
#[derive(Debug, Clone, Copy)]
struct JsToolchain {
    package_manager: PackageManager,
    /// Yarn 2+ ("Berry"), which rejects Yarn 1's `--frozen-lockfile`.
    yarn_berry: bool,
    /// Run `corepack enable` so the version pinned by `packageManager` (or
    /// pnpm itself, which the Node image does not ship) is used.
    corepack: bool,
}

impl JsToolchain {
    fn detect(local_path: &Path, package_manager: PackageManager) -> Self {
        let package_manager_field = read_planning_file(&local_path.join("package.json"))
            .and_then(|manifest| serde_json::from_str::<serde_json::Value>(&manifest).ok())
            .and_then(|manifest| {
                manifest
                    .get("packageManager")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        let has_yarnrc_yml = local_path.join(".yarnrc.yml").is_file();
        let yarn_berry = matches!(package_manager, PackageManager::Yarn)
            && (has_yarnrc_yml
                || package_manager_field
                    .as_deref()
                    .and_then(|field| field.strip_prefix("yarn@"))
                    .and_then(|version| version.split('.').next())
                    .and_then(|major| major.parse::<u32>().ok())
                    .is_some_and(|major| major >= 2)
                || read_planning_file(&local_path.join("yarn.lock"))
                    .is_some_and(|lock| lock.contains("__metadata:")));
        // The Bun image has no corepack; it runs `bun install` regardless.
        let corepack = !matches!(package_manager, PackageManager::Bun)
            && (matches!(package_manager, PackageManager::Pnpm)
                || package_manager_field.is_some()
                || has_yarnrc_yml);
        Self {
            package_manager,
            yarn_berry,
            corepack,
        }
    }

    fn install_command(&self) -> &'static str {
        if self.yarn_berry {
            "yarn install --immutable"
        } else {
            self.package_manager.install_command()
        }
    }
}

/// Directories the install step reads, copied ahead of it when present:
/// Yarn Berry's pinned release, plugins and patches, and the `patches/`
/// directory used by pnpm `patchedDependencies` and `patch-package`.
///
/// A directory COPY cannot use the glob trick the file COPY uses (a glob that
/// matches a directory copies its *contents*), so each line is emitted only
/// for a directory that exists and that `.dockerignore` does not exclude.
fn install_directory_copies(local_path: &Path) -> String {
    let ignore = read_planning_file(&local_path.join(".dockerignore")).unwrap_or_default();
    [
        ".yarn/releases",
        ".yarn/plugins",
        ".yarn/patches",
        "patches",
    ]
    .into_iter()
    .filter(|relative| {
        std::fs::symlink_metadata(local_path.join(relative)).is_ok_and(|metadata| metadata.is_dir())
            && !dockerignore_may_exclude(&ignore, relative)
    })
    .map(|relative| format!("COPY {relative} ./{relative}\n"))
    .collect()
}

/// Conservative `.dockerignore` check: `true` when any rule could exclude
/// `relative` or one of its parent directories. A false positive only skips
/// an optimisation (the full source COPY still runs before the build); a
/// false negative would make the generated COPY fail, so uncertain rules —
/// negations included — count as excluding.
fn dockerignore_may_exclude(ignore: &str, relative: &str) -> bool {
    let segments: Vec<&str> = relative.split('/').collect();
    let ancestors: Vec<String> = (1..=segments.len())
        .map(|length| segments[..length].join("/"))
        .collect();
    ignore
        .lines()
        .map(str::trim)
        .filter(|rule| !rule.is_empty() && !rule.starts_with('#'))
        .any(|rule| {
            let rule = rule.trim_start_matches('!');
            let rule = rule.trim_start_matches("./").trim_start_matches('/');
            let rule = rule
                .trim_end_matches("/**")
                .trim_end_matches("/*")
                .trim_end_matches('/');
            if let Some(rest) = rule.strip_prefix("**/") {
                return segments.iter().any(|segment| may_match(rest, segment));
            }
            ancestors.iter().any(|ancestor| may_match(rule, ancestor))
        })
}

/// `true` when `pattern` equals `candidate`, or `pattern` contains a glob
/// metacharacter and its literal prefix is a prefix of `candidate`.
fn may_match(pattern: &str, candidate: &str) -> bool {
    match pattern.find(['*', '?', '[']) {
        Some(index) => candidate.starts_with(&pattern[..index]),
        None => pattern == candidate,
    }
}

/// Vite config file names, in Vite's own resolution order.
const VITE_CONFIG_FILES: [&str; 6] = [
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.ts",
    "vite.config.cjs",
    "vite.config.mts",
    "vite.config.cts",
];

/// Output directory for the nginx stage, relative to the app directory.
///
/// Precedence: an explicit override (`.temps.yaml`, then project settings,
/// resolved by the caller), then a plain string-literal `build.outDir` in the
/// Vite config, then Vite's default `dist`.
fn resolve_output_dir(
    configured: Option<&str>,
    local_path: &Path,
    warnings: &mut Vec<String>,
) -> String {
    if let Some(configured) = configured {
        return configured.to_string();
    }
    let Some((file, contents)) = VITE_CONFIG_FILES
        .iter()
        .find_map(|name| read_planning_file(&local_path.join(name)).map(|text| (*name, text)))
    else {
        return "dist".to_string();
    };
    match parse_vite_out_dir(&contents) {
        OutDir::Literal(dir) => {
            tracing::info!(file, out_dir = %dir, "Using build.outDir from the Vite config");
            dir
        }
        OutDir::Absent => "dist".to_string(),
        OutDir::Unresolvable(reason) => {
            let message = format!(
                "{file} sets build.outDir to a value Temps cannot read statically ({reason}); \
                 assuming 'dist'. If the build output is elsewhere, set the output directory \
                 in the project's build settings or in .temps.yaml (build.output_dir)."
            );
            tracing::warn!("{message}");
            warnings.push(message);
            "dist".to_string()
        }
    }
}

/// What a conservative read of a Vite config found for `build.outDir`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutDir {
    /// A single plain string literal that is a safe relative path.
    Literal(String),
    /// No `build.outDir` key.
    Absent,
    /// Present but not statically known (expression, several values, unsafe).
    Unresolvable(&'static str),
}

/// Lexical token of the JavaScript subset this reader understands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Str(String),
    /// A template literal containing `${...}`.
    Template,
    Punct(char),
}

/// Tokenize JS/TS source, skipping whitespace and comments. Strings are read
/// with their escapes so a `//` inside a URL is not mistaken for a comment.
fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, '\'' | '"' | '`') {
            let quote = c;
            let mut value = String::new();
            let mut interpolated = false;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    if let Some(next) = chars.get(i + 1) {
                        value.push(*next);
                    }
                    i += 2;
                    continue;
                }
                if quote == '`' && chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
                    interpolated = true;
                }
                value.push(chars[i]);
                i += 1;
            }
            i += 1;
            tokens.push(if interpolated {
                Token::Template
            } else {
                Token::Str(value)
            });
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let begin = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            tokens.push(Token::Word(chars[begin..i].iter().collect()));
        } else {
            tokens.push(Token::Punct(c));
            i += 1;
        }
    }
    tokens
}

/// What one object literal is known to set a tracked key to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Absent,
    Literal(String),
    Unknown(&'static str),
}

impl Slot {
    /// A spread, computed key or later argument after a literal may replace
    /// it, so the literal can no longer be trusted.
    fn overridden(&mut self, reason: &'static str) {
        if matches!(self, Slot::Literal(_)) {
            *self = Slot::Unknown(reason);
        }
    }
}

/// What an open `{`, `(` or `[` is, as far as finding the config goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// An object literal.
    Object,
    /// A statement block or function body.
    Block,
    /// A parenthesised expression, or a call that may receive the config
    /// itself (`defineConfig(...)`, `mergeConfig(...)`, `export default wrap(...)`).
    Paren,
    /// An array literal, computed key, or the arguments of any other call.
    Opaque,
}

/// One open `{`, `(` or `[` while scanning a Vite config.
struct Frame {
    kind: FrameKind,
    /// An object literal opened directly inside this frame would be a Vite
    /// config (it is not nested in another object, array or plugin call).
    config_scope: bool,
    /// This object literal is a candidate Vite config.
    config: bool,
    /// This object literal is the `build` value of a candidate Vite config.
    config_build: bool,
    /// For the config's `build` object: what its own `outDir` key resolves to.
    out_dir: Slot,
    /// For a config object: what its (last) `build` key resolves `outDir` to.
    build: Slot,
}

impl Frame {
    fn new(kind: FrameKind, config_scope: bool) -> Self {
        Self {
            kind,
            config_scope,
            config: false,
            config_build: false,
            out_dir: Slot::Absent,
            build: Slot::Absent,
        }
    }

    /// An object literal; `in_config_scope` is the parent's `config_scope`.
    fn object(in_config_scope: bool, config_build: bool) -> Self {
        Self {
            config: in_config_scope,
            config_build,
            ..Self::new(FrameKind::Object, false)
        }
    }

    /// Whether members of this object can change the resolved `build.outDir`.
    fn tracked(&self) -> bool {
        self.config || self.config_build
    }
}

const SPREAD_OVERRIDE: &str =
    "build.outDir may be replaced by a spread or computed key that follows it";

/// Words after which `{` starts an object literal rather than a block.
const OBJECT_AFTER: &[&str] = &[
    "return", "default", "yield", "await", "typeof", "void", "case", "throw", "in", "of", "delete",
];

/// Words after which `(` groups an expression or a parameter list rather than
/// calling a function.
const GROUPING_AFTER: &[&str] = &[
    "return", "default", "yield", "await", "typeof", "void", "case", "throw", "in", "of", "delete",
    "async", "if", "while", "for", "switch", "catch", "function",
];

/// Whether a `{` preceded by `prev` opens an object literal (as opposed to a
/// function body, `if`/`else` block or other statement block).
fn opens_object(prev: Option<&Token>) -> bool {
    match prev {
        Some(Token::Punct(c)) => matches!(c, '(' | ',' | '=' | ':' | '?' | '[' | '|' | '&' | '!'),
        Some(Token::Word(word)) => OBJECT_AFTER.contains(&word.as_str()),
        _ => false,
    }
}

/// Whether the `(` at `index` keeps the config scope of its parent: a plain
/// grouping/parameter paren, or a call that receives the config itself. Any
/// other call (`somePlugin({ build })`) takes options that are not Vite's.
fn paren_keeps_config_scope(tokens: &[Token], index: usize) -> bool {
    let before = |offset: usize| index.checked_sub(offset).and_then(|i| tokens.get(i));
    match before(1) {
        Some(Token::Word(word)) if GROUPING_AFTER.contains(&word.as_str()) => true,
        Some(Token::Word(callee)) => {
            let exported = matches!(before(2), Some(Token::Word(w)) if w == "default")
                || (matches!(before(2), Some(Token::Punct('=')))
                    && matches!(before(3), Some(Token::Word(w)) if w == "exports"));
            exported || callee.to_ascii_lowercase().contains("config")
        }
        Some(Token::Punct(')' | ']')) | Some(Token::Str(_)) | Some(Token::Template) => false,
        _ => true,
    }
}

/// Hand a closed frame's findings on: the config's `build` object becomes the
/// config's `build` value (a later `build` key replaces an earlier one, as in
/// JavaScript), and a config object that had a `build` key reports it.
fn close_frame(frame: Frame, parent: Option<&mut Frame>, results: &mut Vec<Slot>) {
    if frame.config_build {
        match parent {
            Some(parent) => parent.build = frame.out_dir,
            None => results.push(frame.out_dir),
        }
    } else if frame.config && frame.build != Slot::Absent {
        results.push(frame.build);
    }
}

/// Find `build: { outDir: '<literal>' }` in a Vite config.
///
/// Only the `build` key of a config object counts: an object literal that is
/// not nested in another object, array or plugin call — the argument of
/// `defineConfig(...)`/`mergeConfig(...)`, `export default {...}`,
/// `module.exports = {...}`, or an object returned from a config function
/// (every branch of a ternary or `if` included). A `build` key anywhere else —
/// plugin options (`plugins: [somePlugin({ build })]`), `worker`, `ssr`,
/// `test`, `environments` — is not Vite's build settings and is ignored, as is
/// any `outDir` outside the config's own `build` object (a type declaration
/// plugin's `outDir`, `build.rollupOptions.output.dir`, `build.lib`). The
/// value must be a plain string literal immediately followed by `,` or `}`;
/// anything else (a call such as `resolve(__dirname, 'out')`, a
/// concatenation, a template with interpolation, a variable) is reported as
/// unresolvable rather than guessed.
///
/// Object semantics are respected where they could change the answer: a later
/// duplicate key wins, while a spread (`...shared`) or computed key (`[k]:`)
/// after the literal — in the config's `build` object, or after `build` in the
/// config object — may replace it, so the result is unresolvable. The same
/// holds for a config object passed as a non-final argument
/// (`mergeConfig({..}, x)`) and for a config `build` value that is not an
/// object literal (`build: shared`). A spread *before* the literal is
/// overridden by it and is harmless.
fn parse_vite_out_dir(source: &str) -> OutDir {
    let tokens = tokenize(source);
    let mut frames: Vec<Frame> = Vec::new();
    let mut results: Vec<Slot> = Vec::new();
    let key_at = |index: usize| -> Option<String> {
        match tokens.get(index) {
            Some(Token::Word(word)) | Some(Token::Str(word)) => Some(word.clone()),
            _ => None,
        }
    };
    let punct_at = |index: usize, expected: &[char]| -> bool {
        matches!(tokens.get(index), Some(Token::Punct(c)) if expected.contains(c))
    };
    for (index, token) in tokens.iter().enumerate() {
        let prev = index.checked_sub(1).and_then(|i| tokens.get(i));
        let config_scope = frames.last().is_none_or(|frame| frame.config_scope);
        // The token begins a member (key, spread or computed key) of the
        // config object or of its `build` object.
        let tracked_member = frames.last().is_some_and(Frame::tracked)
            && index > 0
            && punct_at(index - 1, &['{', ',']);
        match token {
            Token::Punct('{') => {
                let frame = if opens_object(prev) {
                    let key = (index >= 2 && punct_at(index - 1, &[':']))
                        .then(|| key_at(index - 2))
                        .flatten();
                    let parent_is_config = frames.last().is_some_and(|frame| frame.config);
                    Frame::object(
                        config_scope,
                        parent_is_config && key.as_deref() == Some("build"),
                    )
                } else {
                    Frame::new(FrameKind::Block, config_scope)
                };
                frames.push(frame);
            }
            Token::Punct('(') => {
                let frame = if paren_keeps_config_scope(&tokens, index) {
                    Frame::new(FrameKind::Paren, config_scope)
                } else {
                    Frame::new(FrameKind::Opaque, false)
                };
                frames.push(frame);
            }
            Token::Punct('[') => {
                if tracked_member {
                    if let Some(frame) = frames.last_mut() {
                        frame.out_dir.overridden(SPREAD_OVERRIDE);
                        frame.build.overridden(SPREAD_OVERRIDE);
                    }
                }
                frames.push(Frame::new(FrameKind::Opaque, false));
            }
            Token::Punct('}' | ')' | ']') => {
                if let Some(mut frame) = frames.pop() {
                    // `mergeConfig({ build }, other)`: a later argument may
                    // replace this config's `build`.
                    let later_argument = frame.config
                        && frames
                            .last()
                            .is_some_and(|parent| parent.kind == FrameKind::Paren)
                        && punct_at(index + 1, &[','])
                        && !punct_at(index + 2, &[')']);
                    if later_argument {
                        frame
                            .build
                            .overridden("a later argument may replace the build options");
                    }
                    close_frame(frame, frames.last_mut(), &mut results);
                }
            }
            Token::Punct('.')
                if tracked_member && punct_at(index + 1, &['.']) && punct_at(index + 2, &['.']) =>
            {
                if let Some(frame) = frames.last_mut() {
                    frame.out_dir.overridden(SPREAD_OVERRIDE);
                    frame.build.overridden(SPREAD_OVERRIDE);
                }
            }
            Token::Word(_) | Token::Str(_) if tracked_member => {
                let Some(frame) = frames.last_mut() else {
                    continue;
                };
                let shorthand = matches!(token, Token::Word(_)) && punct_at(index + 1, &[',', '}']);
                match key_at(index).as_deref() {
                    Some("outDir") if frame.config_build => {
                        if punct_at(index + 1, &[':']) {
                            frame.out_dir = match (tokens.get(index + 2), tokens.get(index + 3)) {
                                (Some(Token::Str(value)), Some(Token::Punct(',' | '}'))) => {
                                    Slot::Literal(value.clone())
                                }
                                _ => Slot::Unknown("build.outDir is not a plain string literal"),
                            };
                        } else if shorthand || punct_at(index + 1, &['(']) {
                            // `{ outDir }` names a variable; `outDir() {}` is a method.
                            frame.out_dir =
                                Slot::Unknown("build.outDir is not a plain string literal");
                        }
                    }
                    Some("build") if frame.config => {
                        if punct_at(index + 1, &[':']) {
                            let plain_value = matches!(
                                tokens.get(index + 2),
                                Some(Token::Str(_)) | Some(Token::Punct('{'))
                            ) || matches!(
                                tokens.get(index + 2),
                                Some(Token::Word(word)) if word == "true" || word == "false"
                            );
                            if !plain_value {
                                frame.build = Slot::Unknown("build is not a plain object literal");
                            }
                        } else if shorthand {
                            frame.build = Slot::Unknown("build is not a plain object literal");
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    // Unbalanced source: still report whatever the open frames found.
    while let Some(frame) = frames.pop() {
        close_frame(frame, frames.last_mut(), &mut results);
    }
    let mut found: Vec<String> = Vec::new();
    for slot in results {
        match slot {
            Slot::Absent => {}
            Slot::Literal(value) => found.push(value),
            Slot::Unknown(reason) => return OutDir::Unresolvable(reason),
        }
    }
    found.sort();
    found.dedup();
    match found.as_slice() {
        [] => OutDir::Absent,
        [value] => match safe_relative_dir(value) {
            Some(dir) => OutDir::Literal(dir),
            None => OutDir::Unresolvable("build.outDir must be a relative path inside the app"),
        },
        _ => OutDir::Unresolvable("build.outDir has more than one value"),
    }
}

/// Normalize `./build/` to `build`; reject absolute, parent-relative and
/// shell/Dockerfile-unsafe paths, since the value lands in a COPY line.
fn safe_relative_dir(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    let valid = !trimmed.is_empty()
        && !trimmed.starts_with('/')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        && trimmed
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    valid.then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DockerfileConfig;

    #[tokio::test]
    async fn nested_pnpm_vite_installs_root_and_builds_dependency_graph() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*, packages/*]",
        )
        .unwrap();
        std::fs::write(repo.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
        let result = Vite
            .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
            .await
            .content;
        assert!(result.contains("corepack enable"));
        assert!(result.contains("COPY . .\nENV CI=true\nRUN pnpm install --frozen-lockfile"));
        assert!(result.contains("pnpm --filter './apps/web...' --if-present run build"));
        assert!(result.contains("/app/apps/web/dist /usr/share/nginx/html"));
    }

    #[tokio::test]
    async fn workspace_overrides_run_install_at_root_and_build_in_app() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: [apps/*]",
        )
        .unwrap();
        let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
        config.install_command = Some("pnpm install && pnpm run prepare");
        config.build_command = Some("pnpm run release");
        config.output_dir = Some("public");
        let result = Vite.dockerfile(config).await.content;
        assert!(result.contains(
            "RUN pnpm install && pnpm run prepare\nWORKDIR /app/apps/web\nRUN pnpm run release"
        ));
        assert!(result.contains("/app/apps/web/public /usr/share/nginx/html"));
    }

    #[tokio::test]
    async fn extglob_vite_members_receive_root_install_and_dependency_builds() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: ['apps/@(web|api|private)', '!apps/@(private|internal)']",
        )
        .unwrap();
        for (name, member) in [
            ("web", true),
            ("api", true),
            ("private", false),
            ("mobile", false),
        ] {
            let app = repo.path().join("apps").join(name);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("package.json"), "{}").unwrap();
            let result = Vite
                .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
                .await
                .content;
            assert_eq!(
                result.contains("pnpm install --frozen-lockfile"),
                member,
                "{name}: {result}"
            );
            assert_eq!(result.contains("--filter"), member, "{name}: {result}");
        }
    }

    #[tokio::test]
    async fn nonmember_vite_apps_never_receive_workspace_filters() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join("package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        for packages in [
            "packages: [packages/*]",
            "packages: ['apps/*', '!apps/web']",
        ] {
            std::fs::write(repo.path().join("pnpm-workspace.yaml"), packages).unwrap();
            let result = Vite
                .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
                .await
                .content;
            assert!(!result.contains("--filter"), "{result}");
            assert!(!result.contains("WORKDIR /app/apps/web"), "{result}");
            assert!(result.contains("RUN npm install"), "{result}");
        }
    }

    #[tokio::test]
    async fn standalone_pnpm_enables_pinned_manager_without_workspace_filter() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").unwrap();
        let result = Vite
            .dockerfile(DockerfileConfig::new(repo.path(), repo.path(), "fixture"))
            .await
            .content;
        assert!(result.contains("RUN corepack enable && pnpm install --frozen-lockfile"));
        assert!(!result.contains("--filter"));
    }

    fn app(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        dir
    }

    const BUILDABLE: &str = r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"6"}}"#;

    async fn render(dir: &Path) -> DockerfileWithArgs {
        Vite.dockerfile(DockerfileConfig::new(dir, dir, "fixture"))
            .await
    }

    #[tokio::test]
    async fn missing_build_script_fails_fast_with_the_classifier_pattern() {
        let dir = app(&[("package.json", r#"{"scripts":{"dev":"vite"}}"#)]);
        let result = render(dir.path()).await;
        let failure = result
            .plan_failure
            .expect("missing build script must fail planning");
        assert!(matches!(
            failure,
            BuildPlanFailure::MissingBuildScript { .. }
        ));
        let message = failure.to_string();
        assert!(
            message.to_lowercase().contains("missing script: build"),
            "{message}"
        );
        assert!(message.contains("\"build\": \"vite build\""), "{message}");
        assert!(result.content.contains("exit 1"), "{}", result.content);
    }

    #[tokio::test]
    async fn custom_build_command_skips_the_build_script_check() {
        let dir = app(&[("package.json", r#"{"scripts":{}}"#)]);
        let mut config = DockerfileConfig::new(dir.path(), dir.path(), "fixture");
        config.build_command = Some("npx vite build");
        let result = Vite.dockerfile(config).await;
        assert!(result.plan_failure.is_none());
        assert!(
            result.content.contains("RUN npx vite build"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn buildable_app_has_no_plan_failure_and_copies_install_config_first() {
        let dir = app(&[
            ("package.json", BUILDABLE),
            (".npmrc", "registry=https://registry.example.com/\n"),
        ]);
        let result = render(dir.path()).await;
        assert!(result.plan_failure.is_none(), "{:?}", result.plan_failure);
        let content = result.content;
        let config_copy = content.find(".npmrc*").expect("npmrc copied");
        let install = content.find("RUN npm install").expect("install step");
        let source = content.find("COPY . .").expect("source copy");
        assert!(config_copy < install && install < source, "{content}");
        assert!(
            content.contains(".yarnrc*") && content.contains("bun.lock*"),
            "{content}"
        );
        assert!(!content.contains("corepack"), "{content}");
    }

    #[tokio::test]
    async fn yarn_berry_uses_immutable_install_corepack_and_copies_yarn_dirs() {
        let dir = app(&[
            (
                "package.json",
                r#"{"packageManager":"yarn@4.5.0","scripts":{"build":"vite build"}}"#,
            ),
            ("yarn.lock", "__metadata:\n  version: 8\n"),
            (".yarnrc.yml", "yarnPath: .yarn/releases/yarn-4.5.0.cjs\n"),
            (".yarn/releases/yarn-4.5.0.cjs", "// release"),
            (".yarn/plugins/plugin.cjs", "// plugin"),
            ("patches/left-pad.patch", "diff"),
        ]);
        let content = render(dir.path()).await.content;
        assert!(
            content.contains("RUN corepack enable && yarn install --immutable"),
            "{content}"
        );
        assert!(!content.contains("--frozen-lockfile"), "{content}");
        let install = content.find("yarn install").unwrap();
        for line in [
            "COPY .yarn/releases ./.yarn/releases",
            "COPY .yarn/plugins ./.yarn/plugins",
            "COPY patches ./patches",
        ] {
            let at = content
                .find(line)
                .unwrap_or_else(|| panic!("{line} missing: {content}"));
            assert!(at < install, "{line} must precede install: {content}");
        }
        assert!(
            !content.contains("COPY .yarn/patches"),
            "absent dir copied: {content}"
        );
    }

    #[tokio::test]
    async fn yarn_classic_keeps_frozen_lockfile_and_package_manager_field_enables_corepack() {
        let classic = app(&[
            ("package.json", BUILDABLE),
            ("yarn.lock", "# yarn lockfile v1\n"),
        ]);
        let content = render(classic.path()).await.content;
        assert!(
            content.contains("RUN yarn install --frozen-lockfile"),
            "{content}"
        );

        let pinned = app(&[
            (
                "package.json",
                r#"{"packageManager":"yarn@1.22.22","scripts":{"build":"vite build"}}"#,
            ),
            ("yarn.lock", "# yarn lockfile v1\n"),
        ]);
        let content = render(pinned.path()).await.content;
        assert!(
            content.contains("RUN corepack enable && yarn install --frozen-lockfile"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn dockerignored_install_dirs_are_not_copied_early() {
        let dir = app(&[
            ("package.json", BUILDABLE),
            ("patches/x.patch", "diff"),
            (".dockerignore", "**/node_modules\npatches/\n"),
        ]);
        let content = render(dir.path()).await.content;
        assert!(!content.contains("COPY patches"), "{content}");
    }

    #[test]
    fn dockerignore_matching_is_conservative() {
        assert!(dockerignore_may_exclude(".yarn\n", ".yarn/releases"));
        assert!(dockerignore_may_exclude(".yarn/*\n", ".yarn/releases"));
        assert!(dockerignore_may_exclude("**/patches\n", "patches"));
        assert!(dockerignore_may_exclude("*\n!package.json\n", "patches"));
        assert!(dockerignore_may_exclude("!patches\n", "patches"));
        assert!(!dockerignore_may_exclude(
            "**/node_modules\n# patches\n",
            "patches"
        ));
        assert!(!dockerignore_may_exclude("dist\n.git\n", ".yarn/releases"));
    }

    #[tokio::test]
    async fn vite_config_out_dir_reaches_the_nginx_stage() {
        for (file, config) in [
            ("vite.config.ts", "export default defineConfig({ build: { outDir: 'build' } })"),
            ("vite.config.mjs", "export default { build: { \"outDir\": \"./build/\", sourcemap: true } }"),
            ("vite.config.mts", "export default defineConfig(({ mode }) => ({\n  plugins: [dts({ outDir: 'types' })],\n  build: {\n    // outDir: 'old',\n    outDir: `build`,\n  },\n}))"),
            ("vite.config.cjs", "module.exports = { server: { proxy: { '/api': 'http://localhost:3000' } }, build: { outDir: 'build' } }"),
        ] {
            let dir = app(&[("package.json", BUILDABLE), (file, config)]);
            let result = render(dir.path()).await;
            assert!(
                result.content.contains("COPY --from=builder /app/build /usr/share/nginx/html"),
                "{file}: {}",
                result.content
            );
            assert!(result.warnings.is_empty(), "{file}: {:?}", result.warnings);
        }
    }

    #[tokio::test]
    async fn explicit_output_dir_beats_vite_config() {
        let dir = app(&[
            ("package.json", BUILDABLE),
            (
                "vite.config.ts",
                "export default { build: { outDir: 'build' } }",
            ),
        ]);
        let mut config = DockerfileConfig::new(dir.path(), dir.path(), "fixture");
        config.output_dir = Some("public");
        let content = Vite.dockerfile(config).await.content;
        assert!(
            content.contains("/app/public /usr/share/nginx/html"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn unreadable_out_dir_falls_back_to_dist_with_a_warning() {
        let dir = app(&[
            ("package.json", BUILDABLE),
            (
                "vite.config.ts",
                "export default { build: { outDir: resolve(__dirname, 'out') } }",
            ),
        ]);
        let result = render(dir.path()).await;
        assert!(
            result.content.contains("/app/dist /usr/share/nginx/html"),
            "{}",
            result.content
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("build.outDir"),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn out_dir_parser_rejects_everything_that_is_not_a_plain_literal() {
        assert_eq!(parse_vite_out_dir("export default {}"), OutDir::Absent);
        assert_eq!(
            parse_vite_out_dir("export default { plugins: [dts({ outDir: 'types' })] }"),
            OutDir::Absent
        );
        assert_eq!(
            parse_vite_out_dir(
                "const u = 'http://x//y'; export default { build: { outDir: 'out' } }"
            ),
            OutDir::Literal("out".into())
        );
        for unresolvable in [
            "export default { build: { outDir: 'a' + suffix } }",
            "export default { build: { outDir: `${base}/out` } }",
            "export default { build: { outDir } }",
            "export default { build: { outDir: mode === 'x' ? 'a' : 'b' } }",
            "export default { build: { outDir: '../outside' } }",
            "export default { build: { outDir: '/abs' } }",
            "export default { build: { outDir: 'a b' } }",
            "export default (m) => m ? { build: { outDir: 'a' } } : { build: { outDir: 'b' } }",
        ] {
            assert!(
                matches!(parse_vite_out_dir(unresolvable), OutDir::Unresolvable(_)),
                "{unresolvable}"
            );
        }
        assert_eq!(
            parse_vite_out_dir(
                "export default (m) => m ? { build: { outDir: 'a' } } : { build: { outDir: 'a' } }"
            ),
            OutDir::Literal("a".into())
        );
    }

    #[test]
    fn out_dir_parser_plain_literal_still_resolves() {
        assert_eq!(
            parse_vite_out_dir("export default defineConfig({ build: { outDir: 'build' } })"),
            OutDir::Literal("build".into())
        );
    }

    #[test]
    fn out_dir_spread_after_the_literal_in_build_is_unresolvable() {
        for config in [
            "const shared = { outDir: 'dist' }; export default { build: { outDir: 'build', ...shared } }",
            "export default { build: { outDir: 'build', sourcemap: true, ...(prod ? a : b) } }",
            "export default { build: { outDir: 'build', [key]: 'dist' } }",
        ] {
            assert!(
                matches!(parse_vite_out_dir(config), OutDir::Unresolvable(_)),
                "{config}"
            );
        }
    }

    #[test]
    fn out_dir_spread_before_the_literal_keeps_the_literal() {
        for config in [
            "const shared = { outDir: 'dist' }; export default { build: { ...shared, outDir: 'build' } }",
            "export default { build: { [key]: 'dist', outDir: 'build' } }",
            "export default { ...base, build: { outDir: 'build' } }",
            "export default { build: { outDir: 'build', rollupOptions: { input: [...pages] }, lib: f(...args) } }",
        ] {
            assert_eq!(
                parse_vite_out_dir(config),
                OutDir::Literal("build".into()),
                "{config}"
            );
        }
    }

    #[test]
    fn out_dir_duplicate_key_takes_the_last_literal() {
        assert_eq!(
            parse_vite_out_dir("export default { build: { outDir: 'first', outDir: 'second' } }"),
            OutDir::Literal("second".into())
        );
        assert_eq!(
            parse_vite_out_dir(
                "export default { build: { outDir: 'first' }, build: { outDir: 'second' } }"
            ),
            OutDir::Literal("second".into())
        );
        assert_eq!(
            parse_vite_out_dir("export default { build: { outDir: 'first' }, build: {} }"),
            OutDir::Absent
        );
    }

    #[test]
    fn out_dir_build_replaced_at_config_level_is_unresolvable() {
        for config in [
            "const other = { build: { outDir: 'dist' } }; export default { build: { outDir: 'build' }, ...other }",
            "export default defineConfig({ build: { outDir: 'build' }, ...other })",
            "export default { build: { outDir: 'build' }, [key]: {} }",
            "export default { build: shared }",
            "export default { build }",
            "export default { build: { outDir: 'build' }, build: shared }",
            "export default mergeConfig({ build: { outDir: 'build' } }, override)",
        ] {
            assert!(
                matches!(parse_vite_out_dir(config), OutDir::Unresolvable(_)),
                "{config}"
            );
        }
        // The last argument of `mergeConfig` wins, so its literal stands.
        assert_eq!(
            parse_vite_out_dir("export default mergeConfig(base, { build: { outDir: 'build' } })"),
            OutDir::Literal("build".into())
        );
    }

    #[test]
    fn out_dir_plugin_build_options_are_not_vite_settings() {
        // A plugin's own `build` option must not make the config unreadable.
        for config in [
            "export default { build: { outDir: 'build' }, plugins: [somePlugin({ build: buildOptions })] }",
            "export default { plugins: [somePlugin({ build: buildOptions })], build: { outDir: 'build' } }",
            "export default defineConfig({ build: { outDir: 'build' }, plugins: [somePlugin({ build: { outDir: 'other', ...rest } })] })",
            "export default { build: { outDir: 'build' }, plugins: [{ name: 'p', config: () => ({ build: shared }) }] }",
            "const plugin = somePlugin({ build: buildOptions });\nexport default { plugins: [plugin], build: { outDir: 'build' } }",
            "export default withWrapper({ build: { outDir: 'build' }, plugins: [p({ build })] })",
        ] {
            assert_eq!(
                parse_vite_out_dir(config),
                OutDir::Literal("build".into()),
                "{config}"
            );
        }
        // A plugin's `build.outDir` alone leaves Vite's default in place.
        for config in [
            "export default { plugins: [somePlugin({ build: { outDir: 'x' } })] }",
            "export default defineConfig({ plugins: [somePlugin({ build: { outDir: 'x' } })] })",
            "const plugin = somePlugin({ build: { outDir: 'x' } });\nexport default { plugins: [plugin] }",
        ] {
            assert_eq!(parse_vite_out_dir(config), OutDir::Absent, "{config}");
        }
    }

    #[test]
    fn out_dir_build_nested_below_the_config_is_ignored() {
        for nested in [
            "worker: { build: shared }",
            "worker: { build: { outDir: 'worker' } }",
            "ssr: { build: { outDir: 'ssr' } }",
            "server: { build }",
            "resolve: { build: x }",
            "test: { build: { outDir: 'coverage' } }",
            "environments: { ssr: { build: { outDir: 'ssr' } } }",
        ] {
            let with_build = format!("export default {{ {nested}, build: {{ outDir: 'build' }} }}");
            assert_eq!(
                parse_vite_out_dir(&with_build),
                OutDir::Literal("build".into()),
                "{with_build}"
            );
            let without_build = format!("export default defineConfig({{ {nested} }})");
            assert_eq!(
                parse_vite_out_dir(&without_build),
                OutDir::Absent,
                "{without_build}"
            );
        }
    }

    #[test]
    fn out_dir_outside_the_config_build_object_is_ignored() {
        for config in [
            "export default { outDir: 'x' }",
            "export default { build: { rollupOptions: { output: { dir: 'y', outDir: 'z' } } } }",
            "export default { build: { lib: { outDir: 'z' }, rollupOptions: { ...shared } } }",
        ] {
            assert_eq!(parse_vite_out_dir(config), OutDir::Absent, "{config}");
        }
    }

    #[test]
    fn out_dir_config_returned_from_a_function_counts() {
        assert_eq!(
            parse_vite_out_dir(
                "export default defineConfig(async ({ mode }) => {\n  const env = loadEnv(mode, process.cwd(), '');\n  return { plugins: [p({ build: env })], build: { outDir: 'out' } };\n})"
            ),
            OutDir::Literal("out".into())
        );
        assert_eq!(
            parse_vite_out_dir(
                "export default defineConfig(({ mode }) => {\n  if (mode === 'a') {\n    return { build: { outDir: 'out' } };\n  }\n  return { build: { outDir: 'out' } };\n})"
            ),
            OutDir::Literal("out".into())
        );
        assert!(matches!(
            parse_vite_out_dir(
                "export default defineConfig(({ mode }) => {\n  if (mode === 'a') {\n    return { build: { outDir: 'a' } };\n  }\n  return { build: { outDir: 'b' } };\n})"
            ),
            OutDir::Unresolvable(_)
        ));
        assert!(matches!(
            parse_vite_out_dir(
                "export default function config() { return { build: { outDir: 'build' }, ...extra } }"
            ),
            OutDir::Unresolvable(_)
        ));
    }

    #[tokio::test]
    async fn plugin_build_option_does_not_change_the_nginx_stage() {
        for (config, expected) in [
            (
                "export default { build: { outDir: 'build' }, plugins: [somePlugin({ build: buildOptions })] }",
                "/app/build /usr/share/nginx/html",
            ),
            (
                "export default { plugins: [somePlugin({ build: { outDir: 'x' } })] }",
                "/app/dist /usr/share/nginx/html",
            ),
        ] {
            let dir = app(&[("package.json", BUILDABLE), ("vite.config.ts", config)]);
            let result = render(dir.path()).await;
            assert!(result.content.contains(expected), "{config}: {}", result.content);
            assert!(result.warnings.is_empty(), "{config}: {:?}", result.warnings);
        }
    }

    #[tokio::test]
    async fn spread_after_out_dir_falls_back_to_dist_with_a_warning() {
        let dir = app(&[
            ("package.json", BUILDABLE),
            (
                "vite.config.ts",
                "const shared = { outDir: 'dist' };\nexport default { build: { outDir: 'build', ...shared } }",
            ),
        ]);
        let result = render(dir.path()).await;
        assert!(
            result.content.contains("/app/dist /usr/share/nginx/html"),
            "{}",
            result.content
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("build.outDir"),
            "{:?}",
            result.warnings
        );
    }

    #[tokio::test]
    async fn invalid_workspace_member_path_is_a_typed_configuration_failure() {
        let repo = tempfile::tempdir().unwrap();
        let app = repo.path().join("apps/we b");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), BUILDABLE).unwrap();
        std::fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages: ['apps/*']",
        )
        .unwrap();
        let result = Vite
            .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
            .await;
        assert!(
            matches!(
                result.plan_failure,
                Some(BuildPlanFailure::InvalidConfiguration { .. })
            ),
            "{:?}",
            result.plan_failure
        );
    }
}
