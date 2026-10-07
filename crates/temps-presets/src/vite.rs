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
/// Alongside each token, whether a line break precedes it, so statement
/// boundaries that rely on automatic semicolon insertion can be found.
fn tokenize(source: &str) -> (Vec<Token>, Vec<bool>) {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut line_breaks = Vec::new();
    let mut line_break = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if tokens.len() > line_breaks.len() {
            line_breaks.push(line_break);
            line_break = false;
        }
        if c.is_whitespace() {
            if c == '\n' {
                line_break = true;
            }
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    line_break = true;
                }
                i += 1;
            }
            i += 2;
        } else if matches!(c, '\'' | '"' | '`') {
            let quote = c;
            let mut value = String::new();
            let mut interpolated = false;
            // An escape (`'\x64ist'`, `'\u0062uild'`) means the runtime value
            // differs from the source text; rather than decode JavaScript
            // escapes, treat the string as non-literal so its value is never
            // guessed.
            let mut escaped = false;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    escaped = true;
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
            tokens.push(if interpolated || escaped {
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
    if tokens.len() > line_breaks.len() {
        line_breaks.push(line_break);
    }
    (tokens, line_breaks)
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
    /// For a config object that is one of several alternatives (a ternary
    /// arm, or a `return` in a config function): the group of alternatives
    /// it belongs to, keyed by its nearest enclosing call or parenthesis.
    /// `None` for anything else: an export, a merged `mergeConfig` input, an
    /// arrow-returned helper object.
    branch_group: Option<usize>,
    /// Unique id; a group of alternative configs is keyed by the id of the
    /// frame that holds them.
    id: usize,
    /// Token index of the opening `{`, `(` or `[`.
    open_index: usize,
    /// A block that is a function body (after `=>` or a parameter list), as
    /// opposed to an `if`/`for`/`else` block.
    function_body: bool,
    /// A function body of the config function itself: passed to a config
    /// call (`defineConfig(() => { ... })`) or exported directly.
    config_body: bool,
    /// A block whose current statement is a `return`.
    returning: bool,
    /// A config call in an alternative position (`c ? defineConfig({..}) :
    /// defineConfig({..})`): the group its config argument belongs to.
    alt_group: Option<usize>,
    /// For a config object: it has at least one member.
    has_members: bool,
    /// For a config object: one of its keys is a Vite top-level option.
    vite_key: bool,
    /// For a config object: it has a spread or computed key, so it may bring
    /// in a `build` this parser cannot see.
    opaque_members: bool,
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
            branch_group: None,
            id: 0,
            open_index: 0,
            function_body: false,
            config_body: false,
            returning: false,
            alt_group: None,
            has_members: false,
            vite_key: false,
            opaque_members: false,
        }
    }

    /// An object literal; `in_config_scope` is the parent's `config_scope`,
    /// `branch_group` its group of alternative configs, if it is one.
    fn object(in_config_scope: bool, config_build: bool, branch_group: Option<usize>) -> Self {
        Self {
            config: in_config_scope,
            config_build,
            branch_group,
            ..Self::new(FrameKind::Object, false)
        }
    }

    /// A config candidate with no `build` key that is recognisably a Vite
    /// config (empty, or using a Vite option), so `vite build` would write to
    /// the default directory if it were the branch taken.
    fn default_dir_branch(&self) -> bool {
        self.config
            && self.branch_group.is_some()
            && self.build == Slot::Absent
            && !self.opaque_members
            && (!self.has_members || self.vite_key)
    }

    /// Whether members of this object can change the resolved `build.outDir`.
    fn tracked(&self) -> bool {
        self.config || self.config_build
    }
}

/// Top-level Vite config options. A build-less object using one of them is a
/// config branch that leaves `build.outDir` at its default.
const VITE_CONFIG_KEYS: &[&str] = &[
    "root",
    "base",
    "mode",
    "define",
    "plugins",
    "build",
    "publicDir",
    "cacheDir",
    "resolve",
    "css",
    "json",
    "esbuild",
    "assetsInclude",
    "logLevel",
    "clearScreen",
    "envDir",
    "envPrefix",
    "appType",
    "server",
    "preview",
    "optimizeDeps",
    "ssr",
    "worker",
    "experimental",
    "test",
    "environments",
    "builder",
];

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

/// Keywords that cannot end an expression: each needs an operand after it
/// (`typeof x`, `await p`, `a instanceof B`, `export default x`), so a line
/// break right after one never ends the statement.
const NEEDS_OPERAND: &[&str] = &[
    "typeof",
    "void",
    "await",
    "new",
    "delete",
    "yield",
    "in",
    "instanceof",
    "of",
    "as",
    "satisfies",
    "keyof",
    "extends",
    "case",
    "default",
    "export",
    "throw",
    "else",
    "do",
];

/// Keywords whose `( ... ) {` opens a control-flow block, not a function body.
const CONTROL_KEYWORDS: &[&str] = &["if", "while", "for", "switch", "catch", "with"];

/// If the `{` at `brace` opens a function body, the index where the
/// function's head starts (`async`, `function name`, or its parameters);
/// `None` for a control-flow or plain block. `paren_open` maps each `)` to
/// its `(`.
fn function_head(
    tokens: &[Token],
    brace: usize,
    paren_open: &std::collections::HashMap<usize, usize>,
) -> Option<usize> {
    let word = |i: usize| match tokens.get(i) {
        Some(Token::Word(w)) => Some(w.as_str()),
        _ => None,
    };
    let punct = |i: usize, c: char| matches!(tokens.get(i), Some(Token::Punct(p)) if *p == c);
    let prev = brace.checked_sub(1)?;
    let mut head = if punct(prev, '>') && prev >= 1 && punct(prev - 1, '=') {
        // Arrow function: `(params) => {` or `param => {`.
        let params_end = prev.checked_sub(2)?;
        if punct(params_end, ')') {
            *paren_open.get(&params_end)?
        } else if word(params_end).is_some() {
            params_end
        } else {
            return None;
        }
    } else if punct(prev, ')') {
        let open = *paren_open.get(&prev)?;
        if open
            .checked_sub(1)
            .and_then(word)
            .is_some_and(|w| CONTROL_KEYWORDS.contains(&w))
        {
            return None;
        }
        // `function name(...) {`, `function (...) {` or a method `name(...) {`.
        let mut head = open;
        if head >= 2 && word(head - 2) == Some("function") && word(head - 1).is_some() {
            head -= 2;
        } else if head >= 1 && word(head - 1) == Some("function") {
            head -= 1;
        }
        head
    } else {
        return None;
    };
    if head >= 1 && word(head - 1) == Some("async") {
        head -= 1;
    }
    Some(head)
}

/// Whether the function whose head starts at `head` is exported as the
/// config (`export default ...`, `module.exports = ...`).
fn exported_function(tokens: &[Token], head: usize) -> bool {
    let word_at = |i: usize, w: &str| matches!(tokens.get(i), Some(Token::Word(x)) if x == w);
    (head >= 1 && word_at(head - 1, "default"))
        || (head >= 2
            && matches!(tokens.get(head - 1), Some(Token::Punct('=')))
            && word_at(head - 2, "exports"))
}

/// The group of a `return {` alternative: the id of the enclosing function
/// body if that is the config function, looking through `if`/`else` blocks.
/// Returns of any other function (a helper) are not config alternatives.
fn return_group(frames: &[Frame]) -> Option<usize> {
    for frame in frames.iter().rev() {
        match frame.kind {
            FrameKind::Block if frame.function_body => {
                return frame.config_body.then_some(frame.id);
            }
            FrameKind::Block => continue,
            _ => return None,
        }
    }
    None
}

/// The group of a ternary arm `? {` / `: {`: the config call or parenthesis
/// that holds the ternary (`defineConfig(() => c ? {..} : {..})`), the config
/// function when the ternary is what it returns, or the top-level export when
/// the ternary is exported directly (`export default c ? {..} : {..}`,
/// `top_level_export`). A ternary anywhere else (inside a helper function, in
/// another top-level statement) is not a config alternative.
fn ternary_group(frames: &[Frame], top_level_export: bool) -> Option<usize> {
    let Some(holder) = frames.last() else {
        return top_level_export.then_some(0);
    };
    let inside_helper = frames
        .iter()
        .any(|frame| frame.kind == FrameKind::Block && frame.function_body && !frame.config_body);
    if inside_helper {
        return None;
    }
    match holder.kind {
        FrameKind::Paren if holder.config_scope => Some(holder.id),
        FrameKind::Block if holder.returning => return_group(frames),
        _ => None,
    }
}

/// Hand a closed frame's findings on: the config's `build` object becomes the
/// config's `build` value (a later `build` key replaces an earlier one, as in
/// JavaScript), and a config object that had a `build` key reports it.
fn close_frame(frame: Frame, parent: Option<&mut Frame>, results: &mut Vec<(Slot, Option<usize>)>) {
    if frame.config_build {
        match parent {
            Some(parent) => parent.build = frame.out_dir,
            None => results.push((frame.out_dir, None)),
        }
    } else if frame.config && frame.build != Slot::Absent {
        results.push((frame.build, frame.branch_group));
    } else if frame.default_dir_branch() {
        // An alternative without `build` (`cond ? { build } : {}`) builds into
        // the default directory; it must take part in the comparison with its
        // sibling alternatives, or a disagreeing branch would be trusted.
        results.push((Slot::Absent, frame.branch_group));
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
    let (tokens, line_breaks) = tokenize(source);
    let mut frames: Vec<Frame> = Vec::new();
    let mut results: Vec<(Slot, Option<usize>)> = Vec::new();
    let mut next_frame_id = 1;
    // The current top-level statement is the config export
    // (`export default ...` / `module.exports = ...`).
    let mut top_level_export = false;
    let mut paren_open: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
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
        if frames.is_empty() {
            // Automatic semicolon insertion: a line that starts a new
            // statement (a name or string after an expression that could have
            // ended) ends the export, even without `;`. A line starting with
            // `?`, `:`, `.`, `(`, `[` or an operator continues it, as in a
            // ternary split over lines.
            let ends_expression = matches!(
                prev,
                Some(Token::Word(_))
                    | Some(Token::Str(_))
                    | Some(Token::Template)
                    | Some(Token::Punct(')' | ']' | '}'))
            ) && !matches!(prev, Some(Token::Word(w)) if NEEDS_OPERAND.contains(&w.as_str()));
            let starts_statement = matches!(
                token,
                Token::Word(_) | Token::Str(_) | Token::Template
            ) && !matches!(token, Token::Word(w) if matches!(w.as_str(), "in" | "instanceof" | "as" | "satisfies"));
            if line_breaks.get(index).copied().unwrap_or(false)
                && ends_expression
                && starts_statement
            {
                top_level_export = false;
            }
            match token {
                Token::Word(word) if word == "default" => {
                    top_level_export = matches!(prev, Some(Token::Word(w)) if w == "export");
                }
                Token::Punct('=') if matches!(prev, Some(Token::Word(w)) if w == "exports") => {
                    top_level_export = true;
                }
                Token::Punct(';') => top_level_export = false,
                Token::Word(word)
                    if matches!(
                        word.as_str(),
                        "const" | "let" | "var" | "function" | "import" | "class" | "export"
                    ) =>
                {
                    top_level_export = false;
                }
                _ => {}
            }
        }
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
                    // `? {` and a ternary `: {` (not a `key: {` inside an
                    // object) open an arm; `return {` one of a function's
                    // alternative results. Sibling alternatives share the
                    // nearest enclosing call/parenthesis (0 at top level).
                    let parent_is_object = frames
                        .last()
                        .is_some_and(|frame| frame.kind == FrameKind::Object);
                    let group = match prev {
                        // The argument of a config call that is itself an
                        // alternative takes that call's group.
                        Some(Token::Punct('(')) => frames
                            .last()
                            .filter(|frame| frame.kind == FrameKind::Paren)
                            .and_then(|frame| frame.alt_group),
                        Some(Token::Word(word)) if word == "return" => return_group(&frames),
                        Some(Token::Punct('?')) => ternary_group(&frames, top_level_export),
                        Some(Token::Punct(':')) if !parent_is_object => {
                            ternary_group(&frames, top_level_export)
                        }
                        _ => None,
                    };
                    Frame::object(
                        config_scope,
                        parent_is_config && key.as_deref() == Some("build"),
                        group,
                    )
                } else {
                    let mut block = Frame::new(FrameKind::Block, config_scope);
                    if let Some(head) = function_head(&tokens, index, &paren_open) {
                        block.function_body = true;
                        let in_config_call = frames
                            .last()
                            .is_some_and(|f| f.kind == FrameKind::Paren && f.config_scope)
                            && !frames.iter().any(|f| {
                                f.kind == FrameKind::Block && f.function_body && !f.config_body
                            });
                        block.config_body = in_config_call || exported_function(&tokens, head);
                    }
                    block
                };
                frames.push(Frame {
                    id: next_frame_id,
                    open_index: index,
                    ..frame
                });
                next_frame_id += 1;
            }
            Token::Punct('(') => {
                let frame = if paren_keeps_config_scope(&tokens, index) {
                    let mut paren = Frame::new(FrameKind::Paren, config_scope);
                    // `? defineConfig(` / `: defineConfig(` / `return defineConfig(`
                    let callee_is_word =
                        index >= 2 && matches!(tokens.get(index - 1), Some(Token::Word(_)));
                    if callee_is_word {
                        let parent_is_object = frames
                            .last()
                            .is_some_and(|frame| frame.kind == FrameKind::Object);
                        paren.alt_group = match tokens.get(index - 2) {
                            Some(Token::Word(word)) if word == "return" => return_group(&frames),
                            Some(Token::Punct('?')) => ternary_group(&frames, top_level_export),
                            Some(Token::Punct(':')) if !parent_is_object => {
                                ternary_group(&frames, top_level_export)
                            }
                            _ => None,
                        };
                    }
                    paren
                } else {
                    Frame::new(FrameKind::Opaque, false)
                };
                frames.push(Frame {
                    id: next_frame_id,
                    open_index: index,
                    ..frame
                });
                next_frame_id += 1;
            }
            Token::Punct('[') => {
                if tracked_member {
                    if let Some(frame) = frames.last_mut() {
                        frame.out_dir.overridden(SPREAD_OVERRIDE);
                        frame.build.overridden(SPREAD_OVERRIDE);
                        frame.opaque_members = true;
                        frame.has_members = true;
                    }
                }
                frames.push(Frame {
                    open_index: index,
                    ..Frame::new(FrameKind::Opaque, false)
                });
            }
            Token::Punct('}' | ')' | ']') => {
                if let Some(mut frame) = frames.pop() {
                    if matches!(token, Token::Punct(')')) {
                        paren_open.insert(index, frame.open_index);
                    }
                    // `({ mode }) =>` and `{ a } = x` destructure; they are
                    // patterns, not config branches.
                    if frame.kind == FrameKind::Object
                        && (punct_at(index + 1, &['='])
                            || (punct_at(index + 1, &[')']) && punct_at(index + 2, &['='])))
                    {
                        frame.branch_group = None;
                    }
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
                    frame.opaque_members = true;
                    frame.has_members = true;
                }
            }
            Token::Word(_) | Token::Str(_) if tracked_member => {
                let Some(frame) = frames.last_mut() else {
                    continue;
                };
                if frame.config {
                    frame.has_members = true;
                    if key_at(index).is_some_and(|key| VITE_CONFIG_KEYS.contains(&key.as_str())) {
                        frame.vite_key = true;
                    }
                }
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
            Token::Word(word) if word == "return" => {
                if let Some(frame) = frames.last_mut() {
                    if frame.kind == FrameKind::Block {
                        frame.returning = true;
                    }
                }
            }
            Token::Punct(';') => {
                if let Some(frame) = frames.last_mut() {
                    frame.returning = false;
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
    // Groups of alternatives in which some arm sets `outDir`, and groups in
    // which some arm leaves it at the default. An alternative only disagrees
    // with its own siblings: a merged input or an unrelated helper object is
    // never an alternative to the exported config.
    let mut literal_groups: Vec<usize> = Vec::new();
    let mut default_groups: Vec<usize> = Vec::new();
    for (slot, group) in results {
        match slot {
            Slot::Absent => default_groups.extend(group),
            Slot::Literal(value) => {
                literal_groups.extend(group);
                found.push(value);
            }
            Slot::Unknown(reason) => return OutDir::Unresolvable(reason),
        }
    }
    found.sort();
    found.dedup();
    if default_groups
        .iter()
        .any(|group| literal_groups.contains(group))
    {
        return OutDir::Unresolvable(
            "build.outDir differs between config branches: one leaves it at the default",
        );
    }
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

    /// REGRESSION (Greptile on #1295): an escaped string's runtime value
    /// differs from its source text, so it must not be trusted as a path.
    #[test]
    fn out_dir_with_escapes_is_unresolvable() {
        for config in [
            r"export default { build: { outDir: '\x64ist' } }",
            r"export default { build: { outDir: 'b\u0075ild' } }",
            r"export default { build: { outDir: 'my\'dir' } }",
        ] {
            assert!(
                matches!(parse_vite_out_dir(config), OutDir::Unresolvable(_)),
                "{config}"
            );
        }
        // An escape elsewhere in the file does not matter.
        assert_eq!(
            parse_vite_out_dir(r"const s = 'a\nb'; export default { build: { outDir: 'build' } }"),
            OutDir::Literal("build".into())
        );
    }

    /// REGRESSION (Greptile on #1295): a config branch without `build`
    /// builds into the default directory, so it disagrees with a branch that
    /// sets `outDir` and the result must be unresolvable.
    #[test]
    fn out_dir_branch_without_build_takes_part_in_the_comparison() {
        for config in [
            "export default defineConfig(({ command }) => command === 'serve' ? { build: { outDir: 'build' } } : {})",
            "export default defineConfig(({ command }) => command === 'build' ? {} : { build: { outDir: 'build' } })",
            "export default defineConfig(({ mode }) => mode === 'x' ? { build: { outDir: 'build' } } : { plugins: [react()] })",
            "export default defineConfig(({ mode }) => { if (mode === 'x') { return { server: { port: 1 } } } return { build: { outDir: 'build' } } })",
            "export default defineConfig(({ mode }) => { if (mode === 'x') return {}; else return { build: { outDir: 'build' } } })",
            "export default defineConfig(async ({ mode }) => { const env = loadEnv(mode, process.cwd()); return mode === 'x' ? {} : { build: { outDir: 'build' } } })",
            "export default function ({ mode }) { if (mode === 'x') { return { plugins: [] } } return { build: { outDir: 'build' } } }",
            "module.exports = (env) => { if (env.x) { return {} } return { build: { outDir: 'build' } } }",
            // REGRESSION (Greptile on #1295): a ternary exported directly is
            // the config, so its default arm takes part in the comparison.
            "export default process.env.NODE_ENV === 'production' ? {} : { build: { outDir: 'build' } }",
            "export default process.env.CI ? { build: { outDir: 'build' } } : { plugins: [] };",
            "module.exports = process.env.CI ? {} : { build: { outDir: 'build' } }",
            "export default (process.env.CI ? {} : { build: { outDir: 'build' } })",
            // A ternary split over lines (Prettier's style) is one statement.
            "export default process.env.CI\n  ? {}\n  : { build: { outDir: 'build' } }",
            "module.exports =\n  process.env.CI\n    ? { plugins: [] }\n    : { build: { outDir: 'build' } }",
            // REGRESSION (Greptile on #1295): a unary keyword needs an operand,
            // so a line break after it continues the exported expression.
            "export default typeof\n  window === 'undefined' ? {} : { build: { outDir: 'build' } }",
            "export default await\n  isCi() ? {} : { build: { outDir: 'build' } }",
            "export default void 0, typeof\n  process === 'object' ? { plugins: [] } : { build: { outDir: 'build' } }",
            "export default process.env.CI ? defineConfig({}) : defineConfig({ build: { outDir: 'build' } })",
        ] {
            assert!(
                matches!(parse_vite_out_dir(config), OutDir::Unresolvable(_)),
                "{config}"
            );
        }
        // Branches that agree still resolve.
        assert_eq!(
            parse_vite_out_dir(
                "export default defineConfig(({ mode }) => mode === 'x' ? { build: { outDir: 'build' } } : { build: { outDir: 'build' } })"
            ),
            OutDir::Literal("build".into())
        );
    }

    /// Objects that are not config branches never count as one.
    #[test]
    fn non_config_objects_do_not_count_as_default_branches() {
        for config in [
            // REGRESSION (Greptile on #1295): merged inputs are combined, not
            // alternatives, and an unrelated helper object is not a branch.
            "export default mergeConfig({ server: { port: 3000 } }, { build: { outDir: 'build' } })",
            "const defaults = () => ({})\nexport default { build: { outDir: 'build' } }",
            "function defaults() { return {} }\nexport default { build: { outDir: 'build' } }",
            "export default defineConfig(({ mode }) => mergeConfig({ plugins: [] }, { build: { outDir: 'build' } }))",
            // REGRESSION (Greptile on #1295): a helper's returns are not the
            // config function's alternatives, wherever the helper is defined.
            "export default defineConfig(() => { const base = () => { return {} }; return { build: { outDir: 'build' } } })",
            "export default defineConfig(() => { function base() { return { plugins: [] } } return { build: { outDir: 'build' } } })",
            "export default defineConfig(() => { const pick = (c) => c ? {} : { server: {} }; return { build: { outDir: 'build' } } })",
            "function defaults() { return {} }\nexport default defineConfig(({ mode }) => mode === 'x' ? { build: { outDir: 'build' } } : { build: { outDir: 'build' } })",
            "const pick = process.env.CI ? {} : { server: {} };\nexport default { build: { outDir: 'build' } }",
            "export default { build: { outDir: 'build' } }\nconst unused = process.env.CI ? {} : { plugins: [] }",
            // REGRESSION (Greptile on #1295): a semicolon-free export ends at
            // the line break JavaScript turns into a semicolon.
            "export default c ? { build: { outDir: 'build' } } : { build: { outDir: 'build' } }\nglobalThis.extraConfig = c ? {} : { plugins: [] }",
            "module.exports = c ? { build: { outDir: 'build' } } : { build: { outDir: 'build' } }\nwindow.x = c ? {} : { server: {} }",
            "export default c ? { build: { outDir: 'build' } } : { build: { outDir: 'build' } } /* end\n */ foo = c ? {} : { plugins: [] }",
            "const pkg = { name: 'app' }; export default { build: { outDir: 'build' } }",
            "function helper() { return { name: 'p', apply: 'build' } }\nexport default { build: { outDir: 'build' } }",
            "const shared = { plugins: [] }; export default { ...shared, build: { outDir: 'build' } }",
            "export default defineConfig(({ mode }) => mode === 'x' ? { build: { outDir: 'build' } } : { ...base, build: { outDir: 'build' } })",
        ] {
            assert_eq!(
                parse_vite_out_dir(config),
                OutDir::Literal("build".into()),
                "{config}"
            );
        }
        assert_eq!(
            parse_vite_out_dir("export default defineConfig({ plugins: [react()] })"),
            OutDir::Absent
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
