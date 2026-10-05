// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The files a JavaScript workspace install needs, so a generated Dockerfile
//! can copy just those before installing dependencies.
//!
//! Copying the whole repository before `install` puts every source file into
//! the install layer's cache key: any commit, even one touching a single
//! component, reinstalls every dependency of every workspace package. Copying
//! only the manifests and lockfiles first keeps the install layer cached until
//! a dependency actually changes.
//!
//! That is only safe when the install does not read anything else. A
//! workspace package with an install lifecycle script (`postinstall:
//! "prisma generate"` is the classic one) may need its sources during the
//! install, so [`collect`] returns a [`Fallback`] instead and the caller keeps
//! copying the whole repository first.

use std::path::{Path, PathBuf};

/// Directories that never contain workspace packages and can be huge.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".next",
    ".turbo",
    ".vercel",
    ".cache",
    "dist",
    "build",
    "out",
    "coverage",
];

/// Install inputs at the repository root, copied when present. Each one can
/// change what `install` does: lockfiles, workspace layout, registry config
/// and pnpm hooks.
const ROOT_INSTALL_FILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    ".pnpmfile.cjs",
    "yarn.lock",
    ".yarnrc",
    ".yarnrc.yml",
    "bun.lock",
    "bun.lockb",
    "bunfig.toml",
    ".npmrc",
];

/// Directories at the root that `install` reads: Yarn's bundled release and
/// plugins, and patch files applied to dependencies during install.
const ROOT_INSTALL_DIRS: &[&str] = &[
    ".yarn/releases",
    ".yarn/plugins",
    ".yarn/patches",
    "patches",
];

/// Lifecycle scripts npm, pnpm, Yarn and Bun run for workspace packages
/// during `install`.
const INSTALL_LIFECYCLE_SCRIPTS: &[&str] = &["preinstall", "install", "postinstall", "prepare"];

/// Beyond this many workspace manifests the per-file `COPY` lines stop paying
/// for themselves; copy the repository instead.
const MAX_MANIFESTS: usize = 200;

/// Directory depth searched for workspace packages (`apps/web` is depth 2,
/// `packages/group/pkg` depth 3).
const MAX_DEPTH: usize = 5;

/// What to copy before installing, as paths relative to the repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallManifests {
    /// Root-level install files that exist, including `package.json`.
    pub root_files: Vec<String>,
    /// `package.json` of every workspace package below the root, sorted.
    pub package_manifests: Vec<String>,
    /// Root-level directories `install` reads, that exist.
    pub dirs: Vec<String>,
}

/// Why the whole repository must be copied before installing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub reason: String,
}

/// Find the files the dependency install needs in the repository at `root`.
pub fn collect(root: &Path) -> Result<InstallManifests, Fallback> {
    let root_manifest = root.join("package.json");
    if !root_manifest.is_file() {
        return Err(Fallback {
            reason: "the repository root has no package.json".to_string(),
        });
    }
    check_lifecycle_scripts(root, &root_manifest)?;

    let mut package_manifests = Vec::new();
    walk(root, root, 0, &mut package_manifests)?;
    package_manifests.sort();
    for manifest in &package_manifests {
        check_lifecycle_scripts(root, &root.join(manifest))?;
    }

    let mut root_files = vec!["package.json".to_string()];
    root_files.extend(
        ROOT_INSTALL_FILES
            .iter()
            .filter(|name| root.join(name).is_file())
            .map(|name| name.to_string()),
    );
    let dirs = ROOT_INSTALL_DIRS
        .iter()
        .filter(|name| root.join(name).is_dir())
        .map(|name| name.to_string())
        .collect();

    Ok(InstallManifests {
        root_files,
        package_manifests,
        dirs,
    })
}

fn walk(root: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) -> Result<(), Fallback> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| Fallback {
        reason: format!("could not read {}: {e}", dir.display()),
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Never follow symlinks: they can point outside the checkout.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || name.starts_with('.') || SKIPPED_DIRS.contains(&name) {
            continue;
        }
        let path = entry.path();
        if path.join("package.json").is_file() {
            if found.len() >= MAX_MANIFESTS {
                return Err(Fallback {
                    reason: format!("more than {MAX_MANIFESTS} workspace packages"),
                });
            }
            found.push(relative(root, &path.join("package.json")));
        }
        walk(root, &path, depth + 1, found)?;
    }
    Ok(())
}

fn check_lifecycle_scripts(root: &Path, manifest: &Path) -> Result<(), Fallback> {
    let display = relative(root, manifest);
    let content = std::fs::read_to_string(manifest).map_err(|e| Fallback {
        reason: format!("could not read {display}: {e}"),
    })?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| Fallback {
        reason: format!("{display} is not valid JSON: {e}"),
    })?;
    for section in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(dependencies) = json.get(section).and_then(|value| value.as_object()) {
            for (name, value) in dependencies {
                if value.as_str().is_some_and(|value| value.starts_with("file:") || value.starts_with("link:")) {
                    return Err(Fallback { reason: format!("{display} has local dependency `{name}`, which needs its contents during install") });
                }
            }
        }
    }
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else {
        return Ok(());
    };
    for name in INSTALL_LIFECYCLE_SCRIPTS {
        let Some(command) = scripts.get(*name).and_then(|c| c.as_str()) else {
            continue;
        };
        // `prepare: husky` only installs git hooks, and exits cleanly when
        // there is no `.git` directory, as in a Docker build.
        if *name == "prepare" && matches!(command.trim(), "husky" | "husky install") {
            continue;
        }
        return Err(Fallback {
            reason: format!("{display} has a `{name}` script, which may need source files"),
        });
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(PathBuf::from)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn turbo_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{"name":"repo","scripts":{"build":"turbo run build"}}"#,
        );
        write(root, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        write(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - apps/*\n  - packages/*\n",
        );
        write(root, "turbo.json", "{}");
        write(
            root,
            "apps/web/package.json",
            r#"{"name":"web","scripts":{"build":"next build"}}"#,
        );
        write(
            root,
            "apps/web/src/page.tsx",
            "export default function Page() {}",
        );
        write(root, "packages/ui/package.json", r#"{"name":"@repo/ui"}"#);
        dir
    }

    #[test]
    fn local_dependencies_require_sources_before_install() {
        for section in ["dependencies", "devDependencies", "optionalDependencies"] {
            for protocol in ["file:", "link:"] {
                let repo = turbo_repo();
                let manifest = serde_json::json!({section: {"local": format!("{protocol}../../packages/ui")}});
                write(repo.path(), "apps/web/package.json", &manifest.to_string());
                let fallback = collect(repo.path()).unwrap_err();
                assert!(fallback.reason.contains("local dependency `local`"));
            }
        }
    }

    #[test]
    fn collects_root_install_files_and_every_workspace_manifest() {
        let repo = turbo_repo();
        let manifests = collect(repo.path()).unwrap();
        assert_eq!(
            manifests.root_files,
            vec!["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml"]
        );
        assert_eq!(
            manifests.package_manifests,
            vec!["apps/web/package.json", "packages/ui/package.json"]
        );
        assert!(manifests.dirs.is_empty());
    }

    #[test]
    fn skips_dependency_and_build_output_directories() {
        let repo = turbo_repo();
        write(repo.path(), "node_modules/react/package.json", "{}");
        write(repo.path(), "apps/web/node_modules/next/package.json", "{}");
        write(repo.path(), "apps/web/.next/package.json", "{}");
        write(repo.path(), "packages/ui/dist/package.json", "{}");
        let manifests = collect(repo.path()).unwrap();
        assert_eq!(
            manifests.package_manifests,
            vec!["apps/web/package.json", "packages/ui/package.json"]
        );
    }

    #[test]
    fn includes_patch_and_yarn_directories_that_install_reads() {
        let repo = turbo_repo();
        write(repo.path(), "patches/left-pad@1.3.0.patch", "");
        write(repo.path(), ".yarn/releases/yarn-4.9.2.cjs", "");
        write(repo.path(), ".yarn/cache/some.zip", "");
        let manifests = collect(repo.path()).unwrap();
        assert_eq!(manifests.dirs, vec![".yarn/releases", "patches"]);
    }

    #[test]
    fn postinstall_in_a_workspace_package_falls_back_to_full_copy() {
        let repo = turbo_repo();
        write(
            repo.path(),
            "packages/db/package.json",
            r#"{"name":"@repo/db","scripts":{"postinstall":"prisma generate"}}"#,
        );
        let fallback = collect(repo.path()).unwrap_err();
        assert!(
            fallback.reason.contains("packages/db/package.json")
                && fallback.reason.contains("postinstall"),
            "{}",
            fallback.reason
        );
    }

    #[test]
    fn root_prepare_running_husky_is_allowed() {
        let repo = turbo_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"repo","scripts":{"prepare":"husky"}}"#,
        );
        assert!(collect(repo.path()).is_ok());
    }

    #[test]
    fn husky_followed_by_source_dependent_script_falls_back() {
        let repo = turbo_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"scripts":{"prepare":"husky && node scripts/generate.js"}}"#,
        );
        assert!(collect(repo.path()).is_err());
    }

    #[test]
    fn root_prepare_running_anything_else_falls_back() {
        let repo = turbo_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"name":"repo","scripts":{"prepare":"node gen.js"}}"#,
        );
        let fallback = collect(repo.path()).unwrap_err();
        assert!(fallback.reason.contains("`prepare`"), "{}", fallback.reason);
    }

    #[test]
    fn invalid_manifest_falls_back_with_its_path() {
        let repo = turbo_repo();
        write(repo.path(), "packages/ui/package.json", "{ not json");
        let fallback = collect(repo.path()).unwrap_err();
        assert!(
            fallback.reason.contains("packages/ui/package.json"),
            "{}",
            fallback.reason
        );
    }

    #[test]
    fn missing_root_manifest_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect(dir.path()).is_err());
    }

    #[test]
    fn too_many_packages_falls_back() {
        let repo = turbo_repo();
        for i in 0..=MAX_MANIFESTS {
            write(repo.path(), &format!("packages/p{i}/package.json"), "{}");
        }
        let fallback = collect(repo.path()).unwrap_err();
        assert!(
            fallback.reason.contains("workspace packages"),
            "{}",
            fallback.reason
        );
    }
}
