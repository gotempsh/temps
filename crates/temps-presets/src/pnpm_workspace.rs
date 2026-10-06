// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Membership of a selected package in the root pnpm workspace.

use std::io::Read;
use std::path::Path;

#[derive(Default, serde::Deserialize)]
struct Workspace {
    packages: Option<Vec<String>>,
}

/// Match package-directory patterns without enumerating repository contents.
/// Negative patterns exclude packages regardless of their position in the list.
/// An absent packages list includes only the root package, never nested apps.
pub fn pnpm_workspace_contains(contents: &str, relative: &Path) -> Result<bool, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "Invalid pnpm workspace package directory '{}'",
            relative.display()
        ));
    }
    let workspace: Workspace = serde_yaml::from_str::<Option<Workspace>>(contents)
        .map_err(|error| format!("Cannot parse pnpm-workspace.yaml: {error}"))?
        .unwrap_or_default();
    let patterns = workspace.packages.unwrap_or_default();
    // pnpm's glob search excludes hidden directories by default. Retain the
    // selected app context for those paths rather than assume globset's broader
    // wildcard matching implies pnpm membership.
    if relative
        .components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
    {
        return Ok(false);
    }
    let mut included = false;
    let mut excluded = false;
    for pattern in patterns {
        let (exclude, pattern) = match pattern.strip_prefix('!') {
            Some(pattern) => (true, pattern),
            None => (false, pattern.as_str()),
        };
        if ["@(", "+(", "?(", "*(", "!("]
            .iter()
            .any(|syntax| pattern.contains(syntax))
        {
            return Err(format!("Unsupported pnpm workspace package pattern '{pattern}': use ordinary glob or brace patterns"));
        }
        let parts: Vec<_> = pattern
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if pattern.starts_with('/') || parts.contains(&"..") {
            return Err(format!(
                "pnpm workspace package pattern '{pattern}' must stay within the root"
            ));
        }
        let pattern = parts.join("/");
        let glob = globset::GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| {
                format!("Invalid pnpm workspace package pattern '{pattern}': {error}")
            })?;
        if glob.compile_matcher().is_match(relative) {
            if exclude {
                excluded = true;
            } else {
                included = true;
            }
        }
    }
    Ok(included && !excluded)
}

pub(crate) fn app_is_member(root: &Path, app: &Path) -> Result<bool, String> {
    if !app.join("package.json").is_file() {
        return Ok(false);
    }
    let marker = root.join("pnpm-workspace.yaml");
    let metadata = match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect '{}': {error}", marker.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "pnpm workspace file '{}' must be a regular non-symlink file",
            marker.display()
        ));
    }
    let relative = app.strip_prefix(root).map_err(|_| {
        format!(
            "Application '{}' escapes pnpm workspace '{}'",
            app.display(),
            root.display()
        )
    })?;
    let mut contents = String::new();
    std::fs::File::open(&marker)
        .map_err(|error| format!("Cannot read '{}': {error}", marker.display()))?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut contents)
        .map_err(|error| format!("Cannot read '{}': {error}", marker.display()))?;
    if contents.len() > 1024 * 1024 {
        return Err(format!(
            "pnpm workspace file '{}' exceeds the 1 MiB limit",
            marker.display()
        ));
    }
    pnpm_workspace_contains(&contents, relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_honors_positive_negative_and_nested_patterns() {
        let contents =
            "packages: ['apps/*', 'packages/**', '!apps/private', '!packages/internal/**']";
        for directory in ["apps/web", "packages/shared", "packages/group/shared"] {
            assert!(pnpm_workspace_contains(contents, Path::new(directory)).unwrap());
        }
        for directory in [
            "tools/web",
            "apps/nested/web",
            "apps/private",
            "packages/internal/shared",
        ] {
            assert!(!pnpm_workspace_contains(contents, Path::new(directory)).unwrap());
        }
        assert!(!pnpm_workspace_contains(
            "packages: ['!apps/private', 'apps/*']",
            Path::new("apps/private")
        )
        .unwrap());
    }

    #[test]
    fn membership_supports_defaults_braces_and_validation() {
        assert!(
            !pnpm_workspace_contains("sharedWorkspaceLockfile: true", Path::new("tools/web"))
                .unwrap()
        );
        assert!(!pnpm_workspace_contains("packages: []", Path::new("apps/web")).unwrap());
        assert!(
            pnpm_workspace_contains("packages: ['{apps,packages}/*']", Path::new("apps/web"))
                .unwrap()
        );
        for contents in ["", "# workspace config", "packages: ['!apps/private']"] {
            assert!(!pnpm_workspace_contains(contents, Path::new("apps/web")).unwrap());
        }
        assert!(
            pnpm_workspace_contains("packages: ['./apps//./*']", Path::new("apps/web")).unwrap()
        );
        assert!(
            !pnpm_workspace_contains("packages: ['apps/*']", Path::new("apps/.private")).unwrap()
        );
        for contents in [
            "packages: [",
            "packages: false",
            "packages: ['[broken']",
            "packages: ['**', '!apps/@(private|hidden)']",
        ] {
            assert!(pnpm_workspace_contains(contents, Path::new("apps/web")).is_err());
        }
    }
}
