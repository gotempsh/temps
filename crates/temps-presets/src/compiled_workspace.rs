// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Repository context for a nested Go module or Cargo crate.
//!
//! A generated build copies only the selected application. That loses what a
//! compiled workspace resolves relative to it: a Go `replace` pointing at a
//! sibling module, a `go.work` listing the module, a Cargo `path` dependency
//! outside the crate, or an enclosing Cargo workspace the crate belongs to.
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
}

impl CompiledLanguage {
    /// The language an autopack provider builds, when it is one of these.
    pub fn from_autopack_provider(provider: &str) -> Option<Self> {
        match provider {
            "go" => Some(CompiledLanguage::Go),
            "rust" => Some(CompiledLanguage::Cargo),
            _ => None,
        }
    }

    /// The language a preset builds the application at `app` with: fixed for
    /// a language preset, detected the way autopack detects it for an
    /// auto-detecting one. `None` when the build is not Go or Cargo, so a
    /// `go.mod` or `Cargo.toml` the build never reads is never inspected.
    pub fn for_preset(preset: &str, app: &Path) -> Option<Self> {
        match preset {
            "go" | "nixpacks-go" => Some(CompiledLanguage::Go),
            "rust" | "nixpacks-rust" => Some(CompiledLanguage::Cargo),
            "nixpacks" | "autopack" => crate::NixpacksPreset::detect_provider_id(app)
                .as_deref()
                .and_then(Self::from_autopack_provider),
            _ => None,
        }
    }
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

/// The selected Go module or Cargo crate when it needs sibling directories of
/// the repository at `root`; `None` when building it alone is enough.
///
/// Only the manifest of `language`, the one the build uses, is read: a
/// `go.mod` beside a JavaScript application, or a `package.json` beside a Go
/// module, neither fails nor redirects the build.
pub fn compiled_workspace_app(
    root: &Path,
    selected: &Path,
    language: CompiledLanguage,
) -> Result<Option<CompiledWorkspaceApp>, String> {
    if root == selected {
        return Ok(None);
    }
    let relative = selected
        .strip_prefix(root)
        .map_err(|_| "Application directory escapes the source repository".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Application directory escapes the source repository".to_string());
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
        CompiledLanguage::Go | CompiledLanguage::Cargo => return Ok(None),
    };
    let text = relative
        .to_str()
        .ok_or_else(|| "Application directory must be UTF-8".to_string())?;
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
    {
        return Err(format!(
            "Unsupported application directory '{text}' for a repository build: use letters, \
             digits, '/', '_', '.' and '-', or a Dockerfile with an explicit build context"
        ));
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
fn read_manifest(root: &Path, relative: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let path = root.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read '{}': {error}", relative.display())),
    };
    if !metadata.is_file() {
        return Err(format!(
            "'{}' must be a regular non-symlink file",
            relative.display()
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "'{}' exceeds the {MAX_MANIFEST_BYTES} byte planning limit",
            relative.display()
        ));
    }
    let mut contents = String::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_MANIFEST_BYTES).read_to_string(&mut contents))
        .map_err(|error| format!("Cannot read '{}': {error}", relative.display()))?;
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
) -> Result<(), String> {
    let inside = match (root.canonicalize(), root.join(resolved).canonicalize()) {
        (Ok(root), Ok(target)) => target.starts_with(root),
        _ => false,
    };
    if !inside || !is_regular_file(&root.join(resolved).join(manifest)) {
        return Err(format!(
            "Local dependency '{declaration}' of '{}' has no {manifest} inside the source \
             repository (it is missing, or leaves the repository through a symlink). Upload \
             or connect the complete repository, or use a Dockerfile with an explicit build \
             context",
            owner.display()
        ));
    }
    Ok(())
}

fn escapes(declaration: &str, owner: &Path) -> String {
    format!(
        "Local dependency '{declaration}' of '{}' leaves the source repository. Include it \
         inside the repository, or use a Dockerfile with an explicit build context",
        owner.display()
    )
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

/// A Go path argument: the first token, unquoted.
fn go_path_token(text: &str) -> &str {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c| c == '"' || c == '`')
}

fn is_go_local_path(path: &str) -> bool {
    path == "." || path == ".." || path.starts_with("./") || path.starts_with("../")
}

/// `Some(ignore_go_work)` when the Go module at `relative` needs the
/// repository: a local `replace` outside the module, or a `go.work` above or
/// at it that lists it alongside other modules or carries `replace`
/// directives -- building the module alone would drop that `go.work`, and
/// its replacements with it.
fn go_needs_repository(root: &Path, relative: &Path) -> Result<Option<bool>, String> {
    let go_mod = relative.join("go.mod");
    let contents = read_manifest(root, &go_mod)?.unwrap_or_default();
    let mut needs = false;
    for replace in go_directive_lines(&contents, "replace") {
        let Some((_, target)) = replace.split_once("=>") else {
            continue;
        };
        let target = go_path_token(target);
        if Path::new(target).is_absolute() {
            return Err(escapes(target, &go_mod));
        }
        if !is_go_local_path(target) {
            continue;
        }
        let resolved = resolve(relative, target).ok_or_else(|| escapes(target, &go_mod))?;
        require_dependency(root, &resolved, "go.mod", target, &go_mod)?;
        needs |= !resolved.starts_with(relative);
    }

    // Go uses the nearest go.work at or above the module's directory.
    let mut ignore_go_work = false;
    let mut directory = Some(relative.to_path_buf());
    while let Some(current) = directory {
        let go_work = current.join("go.work");
        if let Some(work) = read_manifest(root, &go_work)? {
            let mut member = false;
            let mut outside = false;
            for entry in go_directive_lines(&work, "use") {
                let target = go_path_token(entry);
                let resolved = resolve(&current, target).ok_or_else(|| escapes(target, &go_work))?;
                member |= resolved == relative;
                outside |= !resolved.starts_with(relative);
            }
            let replaces = go_directive_lines(&work, "replace");
            if member {
                // Replacements in go.work apply to every member and resolve
                // relative to the go.work, which is outside a module-only
                // build. Local targets must exist inside the repository.
                for replace in &replaces {
                    let Some((_, target)) = replace.split_once("=>") else {
                        continue;
                    };
                    let target = go_path_token(target);
                    if Path::new(target).is_absolute() {
                        return Err(escapes(target, &go_work));
                    }
                    if !is_go_local_path(target) {
                        continue;
                    }
                    let resolved =
                        resolve(&current, target).ok_or_else(|| escapes(target, &go_work))?;
                    require_dependency(root, &resolved, "go.mod", target, &go_work)?;
                }
                needs |= outside || !replaces.is_empty();
            } else {
                ignore_go_work = true;
            }
            break;
        }
        directory = if current.as_os_str().is_empty() {
            None
        } else {
            Some(current.parent().map(Path::to_path_buf).unwrap_or_default())
        };
    }
    Ok(needs.then_some(ignore_go_work))
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

fn parse_cargo(root: &Path, relative: &Path) -> Result<Option<toml::Table>, String> {
    read_manifest(root, relative)?
        .map(|contents| {
            toml::from_str::<toml::Table>(&contents)
                .map_err(|error| format!("Cannot parse '{}': {error}", relative.display()))
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
fn cargo_needs_repository(root: &Path, relative: &Path) -> Result<bool, String> {
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
    let workspace = parse_cargo(root, &workspace_manifest)?
        .and_then(|manifest| manifest.get("workspace").and_then(toml::Value::as_table).cloned())
        .ok_or_else(|| {
            format!(
                "'{}' names Cargo workspace '{}', which has no [workspace] table in the \
                 source repository",
                cargo_toml.display(),
                workspace_manifest.display()
            )
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
    if listed("members")
        .iter()
        .any(|pattern| member_matches(pattern, &member))
    {
        return Ok(true);
    }
    if path_dependency_outside || explicit.is_some() {
        // Built inside the repository, Cargo would refuse a crate under a
        // workspace that neither lists nor excludes it.
        return Err(format!(
            "'{}' is inside the Cargo workspace at '{}' but is not one of its members. Add \
             '{}' to [workspace] members or exclude in '{}'",
            relative.display(),
            if workspace_root.as_os_str().is_empty() {
                "the repository root".to_string()
            } else {
                workspace_root.display().to_string()
            },
            member.display(),
            workspace_manifest.display()
        ));
    }
    Ok(false)
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
    fn app(root: &tempfile::TempDir) -> Result<Option<CompiledWorkspaceApp>, String> {
        let selected = root.path().join("apps/api");
        let language = if selected.join("go.mod").is_file() {
            CompiledLanguage::Go
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

    #[test]
    fn cargo_non_member_with_sibling_dependency_is_refused_with_a_remedy() {
        let root = repository(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"packages/*\"]\n"),
            (
                "apps/api/Cargo.toml",
                "[package]\nname = \"qa-api\"\nversion = \"0.1.0\"\n[dependencies]\nqa-shared = { path = \"../../packages/shared\" }\n",
            ),
            SHARED_CARGO,
        ]);
        let error = app(&root).unwrap_err();
        assert!(error.contains("not one of its members"), "{error}");
        assert!(error.contains("apps/api"), "{error}");
    }

    #[test]
    fn dependencies_outside_or_missing_from_the_repository_are_refused() {
        let root = repository(&[(
            "apps/api/go.mod",
            "module a\nreplace b => ../../../outside\n",
        )]);
        assert!(app(&root).unwrap_err().contains("leaves the source repository"));
        let root = repository(&[("apps/api/go.mod", "module a\nreplace b => /srv/b\n")]);
        assert!(app(&root).unwrap_err().contains("leaves the source repository"));
        let root = repository(&[(
            "apps/api/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { path = \"../../packages/missing\" }\n",
        )]);
        assert!(app(&root).unwrap_err().contains("has no Cargo.toml"));
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
        assert!(app(&root).unwrap_err().contains("has no go.mod"));
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
        assert!(error.contains("Unsupported application directory"), "{error}");
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
