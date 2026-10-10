// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Repository context for a nested Go module, Cargo crate or Mix project.
//!
//! A generated build copies only the selected application. That loses what a
//! compiled workspace resolves relative to it: a Go `replace` pointing at a
//! sibling module, a `go.work` listing the module, a Cargo `path` dependency
//! outside the crate, an enclosing Cargo workspace the crate belongs to, or
//! an Elixir umbrella's sibling apps, lockfile and configuration.
//! [`compiled_workspace_app`] says when the build needs the repository root
//! instead, and refuses a dependency that would leave the repository.
//!
//! Only manifests are read (each capped at 1 MiB, never through a symlink);
//! nothing is executed.

use std::path::{Component, Path, PathBuf};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The workspace tool whose layout requires the repository context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledLanguage {
    Go,
    Cargo,
    /// A Mix project, typically an app inside an Elixir umbrella.
    Elixir,
}

impl CompiledLanguage {
    /// The language an autopack provider builds, when it is one of these.
    pub fn from_autopack_provider(provider: &str) -> Option<Self> {
        match provider {
            "go" => Some(CompiledLanguage::Go),
            "rust" => Some(CompiledLanguage::Cargo),
            "elixir" => Some(CompiledLanguage::Elixir),
            _ => None,
        }
    }

    /// The language a preset builds the application at `app` with: fixed for
    /// a language preset, detected the way autopack detects it for an
    /// auto-detecting one. `None` when the build is not Go, Cargo or Mix, so
    /// a `go.mod`, `Cargo.toml` or `mix.exs` the build never reads is never
    /// inspected.
    pub fn for_preset(preset: &str, app: &Path) -> Option<Self> {
        match preset {
            "go" | "nixpacks-go" => Some(CompiledLanguage::Go),
            "rust" | "nixpacks-rust" => Some(CompiledLanguage::Cargo),
            "nixpacks-elixir" => Some(CompiledLanguage::Elixir),
            "nixpacks" | "autopack" => crate::NixpacksPreset::detect_provider_id(app)
                .as_deref()
                .and_then(Self::from_autopack_provider),
            _ => None,
        }
    }
}

/// Why a nested Go module, Cargo crate or Mix project cannot be planned for a repository
/// build. Every variant names the directory, manifest or declaration at fault.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompiledWorkspaceError {
    #[error("Application directory '{directory}' escapes the source repository")]
    ApplicationEscapes { directory: String },

    #[error("Application directory '{directory}' must be UTF-8")]
    ApplicationNotUtf8 { directory: String },

    #[error(
        "Unsupported application directory '{directory}' for a repository build: use letters, \
         digits, '/', '_', '.' and '-', or a Dockerfile with an explicit build context"
    )]
    UnsupportedApplicationDirectory { directory: String },

    #[error("Cannot read '{manifest}': {reason}")]
    ManifestUnreadable { manifest: String, reason: String },

    #[error("'{manifest}' must be a regular non-symlink file")]
    ManifestNotRegularFile { manifest: String },

    #[error("'{manifest}' exceeds the {limit} byte planning limit")]
    ManifestTooLarge { manifest: String, limit: u64 },

    #[error("Cannot parse '{manifest}': {reason}")]
    ManifestUnparsable { manifest: String, reason: String },

    #[error(
        "Local dependency '{declaration}' of '{owner}' leaves the source repository. Include \
         it inside the repository, or use a Dockerfile with an explicit build context"
    )]
    DependencyEscapes { declaration: String, owner: String },

    #[error(
        "Local dependency '{declaration}' of '{owner}' has no {manifest} inside the source \
         repository (it is missing, or leaves the repository through a symlink). Upload or \
         connect the complete repository, or use a Dockerfile with an explicit build context"
    )]
    DependencyMissing {
        declaration: String,
        owner: String,
        manifest: String,
    },

    #[error(
        "'{crate_manifest}' names Cargo workspace '{workspace_manifest}', which has no \
         [workspace] table in the source repository"
    )]
    MissingWorkspaceTable {
        crate_manifest: String,
        workspace_manifest: String,
    },
}

/// A selected application that must be built from the repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledWorkspaceApp {
    /// The application directory relative to the repository root, `/`
    /// separated and limited to characters safe in a shell command.
    pub relative: String,
    pub language: CompiledLanguage,
    /// Go only: an enclosing `go.work` does not list this module, so Go
    /// commands must ignore it once the repository is in the build.
    pub ignore_go_work: bool,
}

/// The selected Go module, Cargo crate or Mix project when it needs sibling
/// directories of the repository at `root`; `None` when building it alone is
/// enough.
///
/// Only the manifest of `language`, the one the build uses, is read: a
/// `go.mod` beside a JavaScript application, or a `package.json` beside a Go
/// module, neither fails nor redirects the build.
pub fn compiled_workspace_app(
    root: &Path,
    selected: &Path,
    language: CompiledLanguage,
) -> Result<Option<CompiledWorkspaceApp>, CompiledWorkspaceError> {
    if root == selected {
        return Ok(None);
    }
    let escapes_repository = || CompiledWorkspaceError::ApplicationEscapes {
        directory: selected.display().to_string(),
    };
    let relative = selected
        .strip_prefix(root)
        .map_err(|_| escapes_repository())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(escapes_repository());
    }
    let ignore_go_work = match language {
        CompiledLanguage::Go if is_regular_file(&selected.join("go.mod")) => {
            match go_needs_repository(root, relative)? {
                Some(ignore_go_work) => ignore_go_work,
                None => return Ok(None),
            }
        }
        CompiledLanguage::Cargo if is_regular_file(&selected.join("Cargo.toml")) => {
            if !cargo_needs_repository(root, relative)? {
                return Ok(None);
            }
            false
        }
        CompiledLanguage::Elixir if is_regular_file(&selected.join("mix.exs")) => {
            if !mix_needs_repository(root, relative)? {
                return Ok(None);
            }
            false
        }
        CompiledLanguage::Go | CompiledLanguage::Cargo | CompiledLanguage::Elixir => {
            return Ok(None)
        }
    };
    let text = relative
        .to_str()
        .ok_or_else(|| CompiledWorkspaceError::ApplicationNotUtf8 {
            directory: relative.display().to_string(),
        })?;
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
    {
        return Err(CompiledWorkspaceError::UnsupportedApplicationDirectory {
            directory: text.to_string(),
        });
    }
    Ok(Some(CompiledWorkspaceApp {
        relative: text.to_string(),
        language,
        ignore_go_work,
    }))
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// A manifest inside the repository, or `None` when absent.
fn read_manifest(root: &Path, relative: &Path) -> Result<Option<String>, CompiledWorkspaceError> {
    use std::io::Read;
    let manifest = || relative.display().to_string();
    let unreadable = |error: std::io::Error| CompiledWorkspaceError::ManifestUnreadable {
        manifest: manifest(),
        reason: error.to_string(),
    };
    let path = root.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    };
    if !metadata.is_file() {
        return Err(CompiledWorkspaceError::ManifestNotRegularFile {
            manifest: manifest(),
        });
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(CompiledWorkspaceError::ManifestTooLarge {
            manifest: manifest(),
            limit: MAX_MANIFEST_BYTES,
        });
    }
    let mut contents = String::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_MANIFEST_BYTES).read_to_string(&mut contents))
        .map_err(unreadable)?;
    Ok(Some(contents))
}

/// `base.join(target)` normalised inside the repository, or `None` when it
/// would leave it (an absolute path, or more `..` than `base` has parents).
fn resolve(base: &Path, target: &str) -> Option<PathBuf> {
    if Path::new(target).is_absolute() {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in base.join(target).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

/// A dependency directory that must exist inside the repository, also after
/// following symlinks.
fn require_dependency(
    root: &Path,
    resolved: &Path,
    manifest: &str,
    declaration: &str,
    owner: &Path,
) -> Result<(), CompiledWorkspaceError> {
    let inside = match (root.canonicalize(), root.join(resolved).canonicalize()) {
        (Ok(root), Ok(target)) => target.starts_with(root),
        _ => false,
    };
    if !inside || !is_regular_file(&root.join(resolved).join(manifest)) {
        return Err(CompiledWorkspaceError::DependencyMissing {
            declaration: declaration.to_string(),
            owner: owner.display().to_string(),
            manifest: manifest.to_string(),
        });
    }
    Ok(())
}

fn escapes(declaration: &str, owner: &Path) -> CompiledWorkspaceError {
    CompiledWorkspaceError::DependencyEscapes {
        declaration: declaration.to_string(),
        owner: owner.display().to_string(),
    }
}

/// The directory arguments of a `go.mod`/`go.work` directive, in both its
/// single-line and block forms. Comments are dropped.
fn go_directive_lines<'a>(contents: &'a str, directive: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut in_block = false;
    for raw in contents.lines() {
        let line = raw.split("//").next().unwrap_or_default().trim();
        if in_block {
            if line.starts_with(')') {
                in_block = false;
            } else if !line.is_empty() {
                found.push(line);
            }
            continue;
        }
        let Some(rest) = line.strip_prefix(directive) else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with('(')) {
            continue;
        }
        let rest = rest.trim();
        if let Some(inline) = rest.strip_prefix('(') {
            let inline = inline.trim();
            if let Some(single) = inline.strip_suffix(')') {
                if !single.trim().is_empty() {
                    found.push(single.trim());
                }
            } else {
                in_block = true;
                if !inline.is_empty() {
                    found.push(inline);
                }
            }
        } else if !rest.is_empty() {
            found.push(rest);
        }
    }
    found
}

/// A Go path argument: the first token, with Go string quoting removed so a
/// quoted path keeps its spaces. `None` for an empty or unterminated token,
/// which Go itself rejects.
fn go_path_token(text: &str) -> Option<String> {
    let text = text.trim_start();
    if let Some(raw) = text.strip_prefix('`') {
        return raw.split_once('`').map(|(path, _)| path.to_string());
    }
    if let Some(quoted) = text.strip_prefix('"') {
        let mut path = String::new();
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(path),
                '\\' => path.push(chars.next()?),
                c => path.push(c),
            }
        }
        return None;
    }
    text.split_whitespace().next().map(str::to_string)
}

/// A `replace` directive's replaced module path and its target.
fn go_replacement(directive: &str) -> Option<(String, String)> {
    let (module, target) = directive.split_once("=>")?;
    Some((go_path_token(module)?, go_path_token(target)?))
}

/// Check a local `replace` target declared in `manifest`, which sits in
/// `base`: it must stay inside the repository and hold a module. Returns
/// where it resolves to, or `None` for a module-path (non-local) target.
fn go_local_replacement(
    root: &Path,
    base: &Path,
    target: &str,
    manifest: &Path,
) -> Result<Option<PathBuf>, CompiledWorkspaceError> {
    if Path::new(target).is_absolute() {
        return Err(escapes(target, manifest));
    }
    if !is_go_local_path(target) {
        return Ok(None);
    }
    let resolved = resolve(base, target).ok_or_else(|| escapes(target, manifest))?;
    require_dependency(root, &resolved, "go.mod", target, manifest)?;
    Ok(Some(resolved))
}

/// The `go.work` Go uses for the module at `relative`: the nearest one at or
/// above its directory.
struct GoWork {
    directory: PathBuf,
    path: PathBuf,
    /// Its `use` directives list the module.
    member: bool,
    /// It also lists a module outside the selected one.
    outside: bool,
    replaces: Vec<(String, String)>,
}

fn nearest_go_work(root: &Path, relative: &Path) -> Result<Option<GoWork>, CompiledWorkspaceError> {
    let mut directory = Some(relative.to_path_buf());
    while let Some(current) = directory {
        let path = current.join("go.work");
        if let Some(work) = read_manifest(root, &path)? {
            let mut member = false;
            let mut outside = false;
            for entry in go_directive_lines(&work, "use") {
                let Some(target) = go_path_token(entry) else {
                    continue;
                };
                let resolved = resolve(&current, &target).ok_or_else(|| escapes(&target, &path))?;
                member |= resolved == relative;
                outside |= !resolved.starts_with(relative);
            }
            let replaces = go_directive_lines(&work, "replace")
                .into_iter()
                .filter_map(go_replacement)
                .collect();
            return Ok(Some(GoWork {
                directory: current,
                path,
                member,
                outside,
                replaces,
            }));
        }
        directory = if current.as_os_str().is_empty() {
            None
        } else {
            Some(current.parent().map(Path::to_path_buf).unwrap_or_default())
        };
    }
    Ok(None)
}

fn is_go_local_path(path: &str) -> bool {
    path == "." || path == ".." || path.starts_with("./") || path.starts_with("../")
}

/// `Some(ignore_go_work)` when the Go module at `relative` needs the
/// repository: a local `replace` outside the module, or a `go.work` above or
/// at it that lists it alongside other modules or carries `replace`
/// directives -- building the module alone would drop that `go.work`, and
/// its replacements with it.
fn go_needs_repository(
    root: &Path,
    relative: &Path,
) -> Result<Option<bool>, CompiledWorkspaceError> {
    let go_mod = relative.join("go.mod");
    let contents = read_manifest(root, &go_mod)?.unwrap_or_default();
    let mut needs = false;
    let mut ignore_go_work = false;
    // Replacements in the go.work apply to every member, override the
    // module's own replacement of the same module, and resolve relative to
    // the go.work, which is outside a module-only build.
    let mut overridden = std::collections::HashSet::new();
    match nearest_go_work(root, relative)? {
        Some(work) if work.member => {
            for (module, target) in &work.replaces {
                go_local_replacement(root, &work.directory, target, &work.path)?;
                overridden.insert(module.clone());
            }
            needs |= work.outside || !work.replaces.is_empty();
        }
        Some(_) => ignore_go_work = true,
        None => {}
    }
    for directive in go_directive_lines(&contents, "replace") {
        let Some((module, target)) = go_replacement(directive) else {
            continue;
        };
        if overridden.contains(&module) {
            continue;
        }
        if let Some(resolved) = go_local_replacement(root, relative, &target, &go_mod)? {
            needs |= !resolved.starts_with(relative);
        }
    }
    Ok(needs.then_some(ignore_go_work))
}

/// `mix.exs` without its `#` comments. A `#` inside a string (`"#{...}"`
/// interpolation, say) is kept.
fn strip_elixir_comments(contents: &str) -> String {
    let mut stripped = String::with_capacity(contents.len());
    for line in contents.lines() {
        let mut in_string = false;
        let mut escaped = false;
        for c in line.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else if c == '"' {
                in_string = true;
            } else if c == '#' {
                break;
            }
            stripped.push(c);
        }
        stripped.push('\n');
    }
    stripped
}

/// The string literal at the start of `text` (after whitespace), unescaped.
/// `None` when the value is not a plain literal, such as an expression or an
/// interpolated string, which planning cannot evaluate.
fn elixir_string_literal(text: &str) -> Option<String> {
    let quoted = text.trim_start().strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.push(chars.next()?),
            '#' if chars.as_str().starts_with('{') => return None,
            c => value.push(c),
        }
    }
    None
}

/// The string values of every `key: "..."` keyword entry in `text`.
fn mix_keyword_strings(text: &str, key: &str) -> Vec<String> {
    let needle = format!("{key}:");
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(position) = text[offset..].find(&needle) {
        let start = offset + position;
        offset = start + needle.len();
        let preceded_by_identifier = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        if preceded_by_identifier {
            continue;
        }
        if let Some(value) = elixir_string_literal(&text[offset..]) {
            found.push(value);
        }
    }
    found
}

/// A dependency tuple, `{:name, ...}`: its name and the text inside it.
fn mix_dependency_tuples(text: &str) -> Vec<(String, &str)> {
    let mut tuples = Vec::new();
    let mut offset = 0;
    while let Some(position) = text[offset..].find("{:") {
        let start = offset + position;
        let body = &text[start + 2..];
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        offset = start + 2;
        if name.is_empty() {
            continue;
        }
        // The tuple ends at its matching brace; strings may hold braces.
        let mut depth = 1usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut end = None;
        for (index, c) in body.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            break;
        };
        tuples.push((name, &body[..end]));
    }
    tuples
}

/// Whether a `key: true` entry appears in a dependency tuple's options.
fn mix_flag_enabled(tuple: &str, key: &str) -> bool {
    let needle = format!("{key}:");
    tuple.match_indices(&needle).any(|(start, _)| {
        let preceded_by_identifier = tuple[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !preceded_by_identifier
            && tuple[start + needle.len()..]
                .trim_start()
                .strip_prefix("true")
                .is_some_and(|rest| {
                    !rest
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                })
    })
}

/// Whether the Mix project at `relative` needs the repository: a local
/// dependency outside the project (`path:`, or `in_umbrella: true`, which Mix
/// resolves to the sibling `../<name>`), or a lockfile, configuration, deps
/// or build directory that an umbrella child keeps at the umbrella root.
///
/// The manifest is Elixir code; only literal paths are read, never
/// evaluated. A computed path is left to Mix.
fn mix_needs_repository(root: &Path, relative: &Path) -> Result<bool, CompiledWorkspaceError> {
    let mix_exs = relative.join("mix.exs");
    let contents = strip_elixir_comments(&read_manifest(root, &mix_exs)?.unwrap_or_default());
    let mut needs = false;
    for (name, tuple) in mix_dependency_tuples(&contents) {
        let mut targets: Vec<(String, String)> = mix_keyword_strings(tuple, "path")
            .into_iter()
            .map(|path| (format!("{{:{name}, path: \"{path}\"}}"), path))
            .collect();
        if mix_flag_enabled(tuple, "in_umbrella") {
            targets.push((format!("{{:{name}, in_umbrella: true}}"), format!("../{name}")));
        }
        for (declaration, target) in targets {
            let resolved =
                resolve(relative, &target).ok_or_else(|| escapes(&declaration, &mix_exs))?;
            require_dependency(root, &resolved, "mix.exs", &declaration, &mix_exs)?;
            needs |= !resolved.starts_with(relative);
        }
    }
    // Umbrella children share the umbrella's lockfile, configuration and
    // output directories. A path leaving the repository is not ours to
    // check: the build behaves as it did before, and Mix reports it.
    for key in ["config_path", "lockfile", "deps_path", "build_path"] {
        for path in mix_keyword_strings(&contents, key) {
            if let Some(resolved) = resolve(relative, &path) {
                needs |= !resolved.starts_with(relative);
            }
        }
    }
    Ok(needs)
}

/// Path dependency declarations in a Cargo manifest: every dependency table,
/// per-target dependency tables, `[workspace.dependencies]`, `[patch.*]` and
/// `[replace]`. `[lib]`/`[[bin]]` paths name source files and are ignored.
fn cargo_path_dependencies(manifest: &toml::Table) -> Vec<String> {
    fn collect(table: &toml::Table, found: &mut Vec<String>) {
        for value in table.values() {
            if let Some(path) = value
                .as_table()
                .and_then(|dependency| dependency.get("path"))
                .and_then(toml::Value::as_str)
            {
                found.push(path.to_string());
            }
        }
    }
    let mut found = Vec::new();
    let dependency_tables = |table: &toml::Table, found: &mut Vec<String>| {
        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(dependencies) = table.get(key).and_then(toml::Value::as_table) {
                collect(dependencies, found);
            }
        }
    };
    dependency_tables(manifest, &mut found);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            dependency_tables(target, &mut found);
        }
    }
    if let Some(workspace) = manifest.get("workspace").and_then(toml::Value::as_table) {
        dependency_tables(workspace, &mut found);
    }
    if let Some(patches) = manifest.get("patch").and_then(toml::Value::as_table) {
        for registry in patches.values().filter_map(toml::Value::as_table) {
            collect(registry, &mut found);
        }
    }
    if let Some(replace) = manifest.get("replace").and_then(toml::Value::as_table) {
        collect(replace, &mut found);
    }
    found
}

fn parse_cargo(root: &Path, relative: &Path) -> Result<Option<toml::Table>, CompiledWorkspaceError> {
    read_manifest(root, relative)?
        .map(|contents| {
            toml::from_str::<toml::Table>(&contents).map_err(|error| {
                CompiledWorkspaceError::ManifestUnparsable {
                    manifest: relative.display().to_string(),
                    reason: error.to_string(),
                }
            })
        })
        .transpose()
}

/// Cargo's workspace member syntax: `*` and `?` within one path segment.
///
/// The pattern comes from the repository's `Cargo.toml`, so this runs in
/// constant stack and at most `pattern.len() * text.len()` steps: a
/// recursive matcher would let a pattern of many `*` take exponential time,
/// or overflow the stack and abort the process.
fn glob_segment_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // The last `*` seen, and the text position it is currently matched up to.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(b'?') => {
                p += 1;
                t += 1;
            }
            Some(expected) if *expected == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                // Let the last `*` absorb one more byte and retry after it.
                Some((star_p, star_t)) => {
                    star = Some((star_p, star_t + 1));
                    p = star_p + 1;
                    t = star_t + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|byte| *byte == b'*')
}

fn member_matches(pattern: &str, member: &Path) -> bool {
    let Some(pattern) = resolve(Path::new(""), pattern) else {
        return false;
    };
    let pattern: Vec<_> = pattern.components().collect();
    let member: Vec<_> = member.components().collect();
    pattern.len() == member.len()
        && pattern.iter().zip(&member).all(|(pattern, member)| {
            glob_segment_matches(
                pattern.as_os_str().as_encoded_bytes(),
                member.as_os_str().as_encoded_bytes(),
            )
        })
}

/// Whether the crate at `relative` needs the repository: a path dependency
/// outside the crate, or membership of an enclosing Cargo workspace.
fn cargo_needs_repository(root: &Path, relative: &Path) -> Result<bool, CompiledWorkspaceError> {
    let cargo_toml = relative.join("Cargo.toml");
    let manifest = parse_cargo(root, &cargo_toml)?.unwrap_or_default();
    let mut path_dependency_outside = false;
    for path in cargo_path_dependencies(&manifest) {
        let resolved = resolve(relative, &path).ok_or_else(|| escapes(&path, &cargo_toml))?;
        require_dependency(root, &resolved, "Cargo.toml", &path, &cargo_toml)?;
        path_dependency_outside |= !resolved.starts_with(relative);
    }
    // The crate is its own workspace root: Cargo looks no further.
    if manifest.contains_key("workspace") {
        return Ok(path_dependency_outside);
    }

    // `package.workspace` names the root explicitly; otherwise Cargo uses the
    // nearest enclosing manifest with a [workspace] table.
    let explicit = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str);
    let workspace_root = match explicit {
        Some(path) => Some(resolve(relative, path).ok_or_else(|| escapes(path, &cargo_toml))?),
        None => {
            let mut found = None;
            let mut current = relative.parent();
            while let Some(directory) = current {
                if let Some(candidate) = parse_cargo(root, &directory.join("Cargo.toml"))? {
                    if candidate.contains_key("workspace") {
                        found = Some(directory.to_path_buf());
                        break;
                    }
                }
                current = directory.parent();
            }
            found
        }
    };
    let Some(workspace_root) = workspace_root else {
        return Ok(path_dependency_outside);
    };
    let workspace_manifest = workspace_root.join("Cargo.toml");
    let workspace_toml = parse_cargo(root, &workspace_manifest)?.unwrap_or_default();
    let workspace = workspace_toml
        .get("workspace")
        .and_then(toml::Value::as_table)
        .cloned()
        .ok_or_else(|| CompiledWorkspaceError::MissingWorkspaceTable {
            crate_manifest: cargo_toml.display().to_string(),
            workspace_manifest: workspace_manifest.display().to_string(),
        })?;
    let member = relative
        .strip_prefix(&workspace_root)
        .unwrap_or(relative)
        .to_path_buf();
    let listed = |key: &str| -> Vec<String> {
        workspace
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    if listed("exclude")
        .iter()
        .filter_map(|excluded| resolve(Path::new(""), excluded))
        .any(|excluded| member.starts_with(excluded))
    {
        return Ok(path_dependency_outside);
    }
    // Not listed, but Cargo also makes every in-repository path dependency
    // of a member a member. Whether this crate is one depends on the whole
    // workspace graph, so when it needs the workspace (a sibling dependency,
    // an explicit `package.workspace`, or inherited `workspace = true`
    // settings) build from the repository and let Cargo decide: an automatic
    // member builds, and a crate that is neither gets Cargo's own error
    // naming the workspace and how to list or exclude it. A crate that needs
    // none of it builds alone, and the workspace's paths do not matter.
    let needs_workspace = listed("members")
        .iter()
        .any(|pattern| member_matches(pattern, &member))
        || path_dependency_outside
        || explicit.is_some()
        || inherits_from_workspace(&manifest);
    if needs_workspace {
        // Built inside the workspace, the crate also uses root manifest
        // paths, relative to the workspace root: the `[workspace.dependencies]`
        // it inherits, `[patch]`, `[replace]` and the root package's own
        // dependencies. They must be in the repository too, or the build
        // would start only to fail in Cargo.
        for path in workspace_root_paths(&workspace_toml, &manifest) {
            let resolved = resolve(&workspace_root, &path)
                .ok_or_else(|| escapes(&path, &workspace_manifest))?;
            require_dependency(root, &resolved, "Cargo.toml", &path, &workspace_manifest)?;
        }
    }
    Ok(needs_workspace)
}

/// The root manifest paths a workspace member's build uses: the
/// `[workspace.dependencies]` entries `member` inherits (`name = { workspace
/// = true }` in any of its dependency tables), plus the root's `[patch]`,
/// `[replace]` and, when the root is also a package, its own dependencies.
/// Workspace dependencies no member of this build inherits are not loaded.
fn workspace_root_paths(workspace_toml: &toml::Table, member: &toml::Table) -> Vec<String> {
    let mut inherited = std::collections::HashSet::new();
    let mut tables = vec![member];
    if let Some(targets) = member.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().filter_map(toml::Value::as_table));
    }
    for table in tables {
        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
            for (name, dependency) in table
                .get(key)
                .and_then(toml::Value::as_table)
                .into_iter()
                .flatten()
            {
                if dependency
                    .as_table()
                    .and_then(|dependency| dependency.get("workspace"))
                    .and_then(toml::Value::as_bool)
                    == Some(true)
                {
                    inherited.insert(name.clone());
                }
            }
        }
    }

    let mut paths = Vec::new();
    if let Some(dependencies) = workspace_toml
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        paths.extend(
            dependencies
                .iter()
                .filter(|(name, _)| inherited.contains(*name))
                .filter_map(|(_, dependency)| {
                    dependency
                        .as_table()
                        .and_then(|dependency| dependency.get("path"))
                        .and_then(toml::Value::as_str)
                        .map(str::to_string)
                }),
        );
    }
    // Everything else the root declares, without `[workspace]`: patches,
    // replacements and, for a root package, its own dependencies.
    let mut root_rest = workspace_toml.clone();
    root_rest.remove("workspace");
    if !root_rest.contains_key("package") {
        for key in ["dependencies", "dev-dependencies", "build-dependencies", "target"] {
            root_rest.remove(key);
        }
    }
    paths.extend(cargo_path_dependencies(&root_rest));
    paths
}

/// Whether a manifest takes any setting from its workspace
/// (`version.workspace = true`, `serde = { workspace = true }`,
/// `[lints] workspace = true`, ...), which only resolves inside it.
fn inherits_from_workspace(manifest: &toml::Table) -> bool {
    fn inherits(value: &toml::Value, depth: usize) -> bool {
        match value {
            toml::Value::Table(table) => {
                table.get("workspace").and_then(toml::Value::as_bool) == Some(true)
                    || (depth < 4 && table.values().any(|value| inherits(value, depth + 1)))
            }
            _ => false,
        }
    }
    manifest.iter().any(|(key, value)| key != "workspace" && inherits(value, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        root
    }

    /// `apps/api`, built as the language whose manifest it has.
    fn app(root: &tempfile::TempDir) -> Result<Option<CompiledWorkspaceApp>, CompiledWorkspaceError> {
        let selected = root.path().join("apps/api");
        let language = if selected.join("go.mod").is_file() {
            CompiledLanguage::Go
        } else if selected.join("mix.exs").is_file() {
            CompiledLanguage::Elixir
        } else {
            CompiledLanguage::Cargo
        };
        compiled_workspace_app(root.path(), &selected, language)
    }

    const SHARED_GO: (&str, &str) = ("packages/shared/go.mod", "module example.test/qa/shared\n");
    const SHARED_CARGO: (&str, &str) = (
        "packages/shared/Cargo.toml",
        "[package]\nname = \"qa-shared\"\nversion = \"0.1.0\"\n",
    );

    const UMBRELLA_ROOT: (&str, &str) = (
        "mix.exs",
        "defmodule U.MixProject do\n  use Mix.Project\n  def project, do: [apps_path: \"apps\", deps: []]\nend\n",
    );
    const SHARED_MIX: (&str, &str) = (
        "apps/shared/mix.exs",
        "defmodule Shared.MixProject do\n  use Mix.Project\n  def project, do: [app: :shared]\nend\n",
    );

    fn mix_project(project: &str, deps: &str) -> String {
        format!(
            "defmodule Api.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :api, version: \"0.1.0\"{project}, deps: deps()]\n  end\n\n  def application, do: [mod: {{Api.Application, []}}]\n\n  defp deps do\n    [\n{deps}\n    ]\n  end\nend\n"
        )
    }

    fn elixir_api() -> CompiledWorkspaceApp {
        CompiledWorkspaceApp {
            relative: "apps/api".into(),
            language: CompiledLanguage::Elixir,
            ignore_go_work: false,
        }
    }

    /// #1386: an umbrella child keeps its `in_umbrella` sibling, and the
    /// lockfile, config and deps directories it shares with the umbrella.
    #[test]
    fn elixir_umbrella_children_need_the_repository() {
        let in_umbrella = mix_project("", "      {:shared, in_umbrella: true},\n      {:jason, \"~> 1.4\"}");
        let path = mix_project("", "      {:shared, path: \"../shared\", override: true}");
        let generated_child = mix_project(
            ",\n      build_path: \"../../_build\",\n      config_path: \"../../config/config.exs\",\n      deps_path: \"../../deps\",\n      lockfile: \"../../mix.lock\"",
            "",
        );
        for mix_exs in [&in_umbrella, &path, &generated_child] {
            let root = repository(&[UMBRELLA_ROOT, ("apps/api/mix.exs", mix_exs), SHARED_MIX]);
            assert_eq!(app(&root), Ok(Some(elixir_api())), "{mix_exs}");
        }
    }

    /// A Mix app with only Hex/git dependencies, or local ones inside its own
    /// directory, builds alone -- as do the same declarations in comments.
    #[test]
    fn standalone_mix_apps_build_alone() {
        for deps in [
            "      {:phoenix, \"~> 1.7\"},\n      {:plug, git: \"https://example.test/plug.git\", tag: \"v1\"}",
            "      {:local, path: \"vendor/local\"}",
            "      # {:shared, in_umbrella: true},\n      {:jason, \"~> 1.4\"} # path: \"../../x\"",
            "      {:shared, in_umbrella: false}",
        ] {
            let root = repository(&[
                ("apps/api/mix.exs", &mix_project("", deps)),
                ("apps/api/vendor/local/mix.exs", "defmodule L.MixProject do\nend\n"),
            ]);
            assert_eq!(app(&root), Ok(None), "{deps}");
        }
        // Paths Mix computes at runtime are not evaluated.
        let root = repository(&[(
            "apps/api/mix.exs",
            &mix_project(",\n      deps_path: Path.expand(\"../deps\", __DIR__)", ""),
        )]);
        assert_eq!(app(&root), Ok(None));
    }

    #[test]
    fn elixir_dependencies_missing_or_outside_the_repository_are_refused() {
        // The umbrella sibling was not uploaded.
        let root = repository(&[
            UMBRELLA_ROOT,
            ("apps/api/mix.exs", &mix_project("", "      {:shared, in_umbrella: true}")),
        ]);
        assert_eq!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing {
                declaration: "{:shared, in_umbrella: true}".into(),
                owner: "apps/api/mix.exs".into(),
                manifest: "mix.exs".into(),
            })
        );
        // A path dependency leaving the repository.
        let root = repository(&[(
            "apps/api/mix.exs",
            &mix_project("", "      {:outside, path: \"../../../outside\"}"),
        )]);
        assert_eq!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyEscapes {
                declaration: "{:outside, path: \"../../../outside\"}".into(),
                owner: "apps/api/mix.exs".into(),
            })
        );
        // A shared lockfile outside the repository is Mix's to report.
        let root = repository(&[(
            "apps/api/mix.exs",
            &mix_project(",\n      lockfile: \"../../../mix.lock\"", ""),
        )]);
        assert_eq!(app(&root), Ok(None));
    }

    #[test]
    fn mix_manifest_scanning_handles_strings_and_nesting() {
        let text = strip_elixir_comments(
            "[{:a, path: \"x#y\"}, # {:b, path: \"z\"}\n {:c, \"~> 1.0\", only: [:dev], runtime: false}, {:d, in_umbrella: true}]\n",
        );
        let tuples = mix_dependency_tuples(&text);
        let names: Vec<_> = tuples.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a", "c", "d"]);
        assert_eq!(mix_keyword_strings(tuples[0].1, "path"), ["x#y"]);
        assert!(mix_flag_enabled(tuples[2].1, "in_umbrella"));
        assert!(!mix_flag_enabled("not_in_umbrella: true", "in_umbrella"));
        assert!(!mix_flag_enabled("in_umbrella: true_ish", "in_umbrella"));
        // `elixirc_paths:` is not `path:`, and interpolation is not a literal.
        assert!(mix_keyword_strings("elixirc_paths: \"lib\"", "path").is_empty());
        assert!(mix_keyword_strings("path: \"#{root}/x\"", "path").is_empty());
        // An unterminated tuple ends the scan instead of reading past it.
        assert!(mix_dependency_tuples("{:broken, path: \"x\"").is_empty());
    }

    #[test]
    fn go_replace_of_a_sibling_module_needs_the_repository() {
        for go_mod in [
            "module example.test/qa/api\n\nrequire example.test/qa/shared v0.0.0\nreplace example.test/qa/shared => ../../packages/shared\n",
            "module example.test/qa/api\n\nreplace (\n\texample.test/qa/shared v0.0.0 => \"../../packages/shared\" // local\n)\n",
        ] {
            let root = repository(&[("apps/api/go.mod", go_mod), SHARED_GO]);
            assert_eq!(
                app(&root),
                Ok(Some(CompiledWorkspaceApp {
                    relative: "apps/api".into(),
                    language: CompiledLanguage::Go,
                    ignore_go_work: false,
                })),
                "{go_mod}"
            );
        }
    }

    #[test]
    fn go_work_membership_needs_the_repository_and_other_workspaces_are_ignored() {
        let root = repository(&[
            ("go.work", "go 1.22\n\nuse (\n\t./apps/api\n\t./packages/shared\n)\n"),
            ("apps/api/go.mod", "module example.test/qa/api\n"),
            SHARED_GO,
        ]);
        let selected = app(&root).unwrap().unwrap();
        assert_eq!(selected.language, CompiledLanguage::Go);
        assert!(!selected.ignore_go_work);

        // A go.work that does not list the module: only a sibling replace
        // brings in the repository, and Go must then ignore the workspace.
        let root = repository(&[
            ("go.work", "go 1.22\nuse ./packages/shared\n"),
            (
                "apps/api/go.mod",
                "module example.test/qa/api\nreplace example.test/qa/shared => ../../packages/shared\n",
            ),
            SHARED_GO,
        ]);
        assert!(app(&root).unwrap().unwrap().ignore_go_work);
    }

    #[test]
    fn standalone_go_and_cargo_apps_build_alone() {
        let root = repository(&[
            ("apps/api/go.mod", "module example.test/qa/api\nrequire github.com/example/lib v1.0.0\nreplace github.com/example/lib => github.com/fork/lib v1.0.1\n"),
        ]);
        assert_eq!(app(&root), Ok(None));
        let root = repository(&[(
            "apps/api/Cargo.toml",
            "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[[bin]]\nname = \"qa-api\"\npath = \"../../src/main.rs\"\n[dependencies]\nserde = \"1\"\n",
        )]);
        assert_eq!(app(&root), Ok(None));
    }

    /// Frontend tooling beside a Go module or Cargo crate (a package.json for
    /// CSS or asset builds) must not hide its sibling dependencies when the
    /// build is Go or Cargo.
    #[test]
    fn a_package_json_beside_the_manifest_keeps_sibling_dependencies() {
        let root = repository(&[
            ("apps/api/package.json", "{\"scripts\":{\"css\":\"tailwindcss\"}}"),
            ("apps/api/go.mod", "module a\nreplace b => ../../packages/shared\n"),
            SHARED_GO,
        ]);
        assert_eq!(
            app(&root).unwrap().map(|app| app.language),
            Some(CompiledLanguage::Go)
        );
        let root = repository(&[
            ("apps/api/package.json", "{}"),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { path = \"../../packages/shared\" }\n",
            ),
            SHARED_CARGO,
        ]);
        assert_eq!(
            app(&root).unwrap().map(|app| app.language),
            Some(CompiledLanguage::Cargo)
        );
    }

    /// A manifest the build never reads is never inspected: a JavaScript
    /// application with a stray `go.mod` whose replacement points nowhere
    /// still builds, and an auto-detecting preset only checks the language it
    /// detects.
    #[test]
    fn manifests_of_another_language_are_ignored() {
        let root = repository(&[
            (
                "apps/api/package.json",
                "{\"name\":\"web\",\"scripts\":{\"start\":\"node server.js\"}}",
            ),
            ("apps/api/go.mod", "module a\nreplace b => ../../packages/missing\n"),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { path = \"../../../outside\" }\n",
            ),
        ]);
        let selected = root.path().join("apps/api");
        assert!(compiled_workspace_app(root.path(), &selected, CompiledLanguage::Go).is_err());
        assert!(compiled_workspace_app(root.path(), &selected, CompiledLanguage::Cargo).is_err());
        assert_eq!(CompiledLanguage::for_preset("nixpacks", &selected), None);
        assert_eq!(CompiledLanguage::for_preset("autopack", &selected), None);
        assert_eq!(CompiledLanguage::for_preset("nextjs", &selected), None);

        // Asked about Cargo, a Go module is not a Cargo crate, and the reverse.
        let root = repository(&[("apps/api/go.mod", "module a\nreplace b => ../../packages/missing\n")]);
        let selected = root.path().join("apps/api");
        assert_eq!(
            compiled_workspace_app(root.path(), &selected, CompiledLanguage::Cargo),
            Ok(None)
        );
        assert_eq!(
            CompiledLanguage::for_preset("autopack", &selected),
            Some(CompiledLanguage::Go)
        );
        assert_eq!(
            CompiledLanguage::for_preset("rust", &selected),
            Some(CompiledLanguage::Cargo)
        );
    }

    /// A go.work's `replace` directives apply to its members and resolve from
    /// the go.work, so a member module that builds alone would lose them.
    #[test]
    fn go_work_replacements_keep_the_repository_in_the_build() {
        let root = repository(&[
            (
                "go.work",
                "go 1.22\nuse ./apps/api\nreplace example.test/qa/shared => ./packages/shared\n",
            ),
            ("apps/api/go.mod", "module example.test/qa/api\n"),
            SHARED_GO,
        ]);
        let selected = app(&root).unwrap().expect("needs the repository");
        assert_eq!(selected.language, CompiledLanguage::Go);
        assert!(!selected.ignore_go_work);

        // A version replacement is lost the same way.
        let root = repository(&[
            (
                "go.work",
                "go 1.22\nuse ./apps/api\nreplace (\n\tgithub.com/example/lib => github.com/fork/lib v1.0.1\n)\n",
            ),
            ("apps/api/go.mod", "module example.test/qa/api\n"),
        ]);
        assert!(app(&root).unwrap().is_some());

        // A go.work that lists only the module and replaces nothing changes
        // nothing about building it alone.
        let root = repository(&[
            ("go.work", "go 1.22\nuse ./apps/api\n"),
            ("apps/api/go.mod", "module example.test/qa/api\n"),
        ]);
        assert_eq!(app(&root), Ok(None));

        // A local go.work replacement must stay inside the repository.
        let root = repository(&[
            ("go.work", "go 1.22\nuse ./apps/api\nreplace b => ../outside\n"),
            ("apps/api/go.mod", "module example.test/qa/api\n"),
        ]);
        assert!(app(&root).is_err());
    }

    #[test]
    fn cargo_path_dependency_and_workspace_membership_need_the_repository() {
        let root = repository(&[
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { path = \"../../packages/shared\" }\n",
            ),
            SHARED_CARGO,
        ]);
        assert_eq!(
            app(&root).unwrap().map(|app| app.language),
            Some(CompiledLanguage::Cargo)
        );

        // Workspace member without its own path dependency.
        let root = repository(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"apps/*\", \"packages/shared\"]\n"),
            ("apps/api/Cargo.toml", "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { workspace = true }\n"),
            SHARED_CARGO,
        ]);
        assert_eq!(app(&root).unwrap().unwrap().relative, "apps/api");

        // Excluded from the enclosing workspace: builds alone.
        let root = repository(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"packages/*\"]\nexclude = [\"apps\"]\n"),
            ("apps/api/Cargo.toml", "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n"),
        ]);
        assert_eq!(app(&root), Ok(None));
    }

    /// A member inherits `[workspace.dependencies]` paths, and Cargo loads
    /// root `[patch]` paths, relative to the workspace root. Both are checked
    /// before the build, like the crate's own path dependencies.
    #[test]
    fn cargo_workspace_root_paths_are_checked_for_members() {
        let member = (
            "apps/api/Cargo.toml",
            "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { workspace = true }\n",
        );
        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/*\"]\n[workspace.dependencies]\nqa-shared = { path = \"packages/shared\" }\n",
            ),
            member,
            SHARED_CARGO,
        ]);
        assert_eq!(app(&root).unwrap().unwrap().relative, "apps/api");

        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/*\"]\n[workspace.dependencies]\nqa-shared = { path = \"packages/missing\" }\n",
            ),
            member,
        ]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing { .. })
        ));

        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/*\"]\n[workspace.dependencies]\nqa-shared = { path = \"../outside\" }\n",
            ),
            member,
        ]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyEscapes { .. })
        ));

        // Only inherited entries are loaded: an unrelated missing one does
        // not block a member that never uses it.
        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/*\"]\n[workspace.dependencies]\nqa-shared = { path = \"packages/shared\" }\nunused = { path = \"packages/missing\" }\n",
            ),
            member,
            SHARED_CARGO,
        ]);
        assert_eq!(app(&root).unwrap().unwrap().relative, "apps/api");

        // A standalone crate the workspace does not list, that needs nothing
        // from it, builds alone however broken the workspace's paths are.
        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"packages/*\"]\n[workspace.dependencies]\nunused = { path = \"packages/missing\" }\n[patch.crates-io]\nserde = { path = \"vendor/serde\" }\n",
            ),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n",
            ),
        ]);
        assert_eq!(app(&root), Ok(None));

        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"apps/*\"]\n[patch.crates-io]\nserde = { path = \"vendor/serde\" }\n",
            ),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n",
            ),
        ]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing { .. })
        ));
    }

    /// Cargo makes a member's in-repository path dependencies members too,
    /// so a crate the workspace does not list may still belong to it. When
    /// it needs the workspace it is built from the repository and Cargo
    /// decides; only a crate that needs nothing outside it builds alone.
    #[test]
    fn unlisted_crates_that_need_the_workspace_build_from_the_repository() {
        let root = repository(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"packages/server\"]\n",
            ),
            (
                "packages/server/Cargo.toml",
                "[package]\nname = \"server\"\nversion = \"0.1.0\"\n[dependencies]\nqa-api = { path = \"../../apps/api\" }\n",
            ),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { path = \"../../packages/shared\" }\n",
            ),
            SHARED_CARGO,
        ]);
        assert_eq!(app(&root).unwrap().unwrap().relative, "apps/api");

        // Inherited settings only resolve inside the workspace.
        let root = repository(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"packages/*\"]\n[workspace.package]\nversion = \"0.1.0\"\n"),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion.workspace = true\n",
            ),
        ]);
        assert!(app(&root).unwrap().is_some());

        // Needing nothing from the workspace, it still builds alone.
        let root = repository(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"packages/*\"]\n"),
            ("apps/api/Cargo.toml", "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n"),
        ]);
        assert_eq!(app(&root), Ok(None));
    }

    /// Go string quoting is honoured, so a quoted path keeps its spaces.
    #[test]
    fn quoted_go_paths_keep_their_spaces() {
        assert_eq!(
            go_path_token(r#""../../packages/my shared" v1.0.0"#).as_deref(),
            Some("../../packages/my shared")
        );
        assert_eq!(
            go_path_token("`../my shared`").as_deref(),
            Some("../my shared")
        );
        assert_eq!(go_path_token(r#""a\"b""#).as_deref(), Some("a\"b"));
        assert_eq!(go_path_token(r#""unterminated"#), None);
        assert_eq!(go_path_token("  ../shared v1").as_deref(), Some("../shared"));

        let root = repository(&[
            (
                "apps/api/go.mod",
                "module a\nreplace example.test/shared => \"../../packages/my shared\"\n",
            ),
            ("packages/my shared/go.mod", "module example.test/shared\n"),
        ]);
        assert!(app(&root).unwrap().is_some());
    }

    /// A go.work replacement overrides the member's own replacement of the
    /// same module, so a stale module-level target that no longer exists
    /// does not block a workspace that redirects it.
    #[test]
    fn go_work_replacements_override_the_modules_own() {
        let root = repository(&[
            (
                "go.work",
                "go 1.22\nuse ./apps/api\nreplace example.test/qa/shared => ./packages/shared\n",
            ),
            (
                "apps/api/go.mod",
                "module example.test/qa/api\nreplace example.test/qa/shared => ./shared\n",
            ),
            SHARED_GO,
        ]);
        assert!(app(&root).unwrap().is_some());

        // Not overridden, the missing target is still refused.
        let root = repository(&[
            ("go.work", "go 1.22\nuse ./apps/api\nreplace example.test/other => ./packages/shared\n"),
            (
                "apps/api/go.mod",
                "module example.test/qa/api\nreplace example.test/qa/shared => ./shared\n",
            ),
            SHARED_GO,
        ]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing { ref manifest, .. }) if manifest == "go.mod"
        ));
    }

    #[test]
    fn dependencies_outside_or_missing_from_the_repository_are_refused() {
        let root = repository(&[(
            "apps/api/go.mod",
            "module a\nreplace b => ../../../outside\n",
        )]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyEscapes { .. })
        ));
        let root = repository(&[("apps/api/go.mod", "module a\nreplace b => /srv/b\n")]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyEscapes { .. })
        ));
        let root = repository(&[(
            "apps/api/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { path = \"../../packages/missing\" }\n",
        )]);
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing { ref manifest, .. }) if manifest == "Cargo.toml"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_dependency_symlinked_out_of_the_repository_is_refused() {
        let outside = repository(&[SHARED_GO]);
        let root = repository(&[(
            "apps/api/go.mod",
            "module a\nreplace b => ../../packages/shared\n",
        )]);
        std::os::unix::fs::symlink(
            outside.path().join("packages"),
            root.path().join("packages"),
        )
        .unwrap();
        assert!(matches!(
            app(&root),
            Err(CompiledWorkspaceError::DependencyMissing { ref manifest, .. }) if manifest == "go.mod"
        ));
    }

    #[test]
    fn the_root_application_and_unsafe_directories() {
        let root = repository(&[("go.mod", "module a\n")]);
        assert_eq!(
            compiled_workspace_app(root.path(), root.path(), CompiledLanguage::Go),
            Ok(None)
        );
        let root = repository(&[
            ("apps/my api/go.mod", "module a\nreplace b => ../../packages/shared\n"),
            SHARED_GO,
        ]);
        let error = compiled_workspace_app(
            root.path(),
            &root.path().join("apps/my api"),
            CompiledLanguage::Go,
        )
        .unwrap_err();
        assert_eq!(
            error,
            CompiledWorkspaceError::UnsupportedApplicationDirectory {
                directory: "apps/my api".to_string()
            }
        );
        assert!(
            error.to_string().contains("Unsupported application directory"),
            "{error}"
        );
    }

    #[test]
    fn workspace_member_globs_match_single_segments() {
        assert!(member_matches("apps/*", Path::new("apps/api")));
        assert!(member_matches("./apps/api", Path::new("apps/api")));
        assert!(member_matches("apps/a?i", Path::new("apps/api")));
        assert!(!member_matches("apps/*", Path::new("apps/api/nested")));
        assert!(!member_matches("packages/*", Path::new("apps/api")));
    }

    #[test]
    fn glob_segments_match_like_cargo() {
        let matches = |pattern: &str, text: &str| {
            glob_segment_matches(pattern.as_bytes(), text.as_bytes())
        };
        assert!(matches("*", ""));
        assert!(matches("*", "api"));
        assert!(matches("a*i", "api"));
        assert!(matches("*pi", "api"));
        assert!(matches("a**", "api"));
        assert!(matches("?p?", "api"));
        assert!(matches("*a*b*", "xaybz"));
        assert!(!matches("?", ""));
        assert!(!matches("a*b", "acbd"));
        assert!(!matches("api", "apis"));
        assert!(!matches("apis", "api"));
    }

    /// A hostile `members` pattern from the repository must neither hang the
    /// planner nor overflow its stack.
    #[test]
    fn hostile_member_patterns_are_matched_in_bounded_time() {
        let started = std::time::Instant::now();
        let many_stars = "*".repeat(1_000_000);
        assert!(glob_segment_matches(many_stars.as_bytes(), b""));
        let backtracking = format!("{}b", "*a".repeat(40));
        assert!(!glob_segment_matches(
            backtracking.as_bytes(),
            "a".repeat(200).as_bytes()
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
