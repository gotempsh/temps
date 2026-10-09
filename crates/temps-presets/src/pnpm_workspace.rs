// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Membership of a selected package in the root package-manager workspace.

use picomatch_rs::{compile_matcher, CompileOptions};
use regex::Regex;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

static NUMERIC_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([+-]?[0-9]+)\.\.([+-]?[0-9]+)").unwrap());

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
    workspace_patterns_contains(patterns, relative)
}

/// Match npm, Yarn or Bun workspace declarations in either package.json shape.
pub fn package_workspace_contains(contents: &str, relative: &Path) -> Result<bool, String> {
    let manifest: serde_json::Value = serde_json::from_str(contents)
        .map_err(|error| format!("Cannot parse workspace package.json: {error}"))?;
    let workspaces = manifest.get("workspaces");
    let packages = workspaces.and_then(|value| {
        value
            .as_array()
            .or_else(|| value.get("packages")?.as_array())
    });
    if workspaces.is_some() && packages.is_none() {
        return Err(
            "package.json workspaces must be an array or an object with a packages array"
                .to_string(),
        );
    }
    let patterns = packages
        .into_iter()
        .flatten()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Workspace package patterns must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    workspace_patterns_contains(patterns, relative)
}

fn workspace_patterns_contains(patterns: Vec<String>, relative: &Path) -> Result<bool, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "Invalid workspace package directory '{}'",
            relative.display()
        ));
    }
    let candidate = relative.to_str().ok_or_else(|| {
        format!(
            "workspace package directory '{}' must be UTF-8",
            relative.display()
        )
    })?;
    let mut included = false;
    let mut excluded = false;
    for pattern in patterns {
        // A leading !(...) is a negative extglob, not a list exclusion.
        let (exclude, pattern) = match pattern
            .strip_prefix('!')
            .filter(|rest| !rest.starts_with('('))
        {
            Some(pattern) => (true, pattern),
            None => (false, pattern.as_str()),
        };
        let parts: Vec<_> = pattern
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if pattern.starts_with('/') || parts.contains(&"..") {
            return Err(format!(
                "workspace package pattern '{pattern}' must stay within the root"
            ));
        }
        let pattern = parts.join("/");
        // The upstream compiler recursively compiles groups. Bound input and
        // possible recursion before entering it; counting opening delimiters
        // conservatively bounds nesting without interpreting glob semantics.
        // The matching engine also enforces its default 1M backtrack budget,
        // whose errors must propagate instead of turning exclusions into misses.
        const MAX_PATTERN_BYTES: usize = 64 * 1024;
        const MAX_GROUP_OPENERS: usize = 128;
        // Negative extglobs can compile a dotted suffix both inside their
        // lookahead and again after the group, multiplying compiler work.
        const MAX_NEGATIVE_GROUPS: usize = 8;
        if pattern.len() > MAX_PATTERN_BYTES
            || pattern
                .bytes()
                .filter(|byte| matches!(byte, b'(' | b'{' | b'['))
                .count()
                > MAX_GROUP_OPENERS
            || pattern.matches("!(").count() > MAX_NEGATIVE_GROUPS
        {
            return Err(format!("workspace package pattern exceeds the {MAX_PATTERN_BYTES} byte, {MAX_GROUP_OPENERS} group or {MAX_NEGATIVE_GROUPS} negative-group compile limit"));
        }
        // The upstream brace expander adds one to the unsigned distance between
        // signed endpoints. Reject that addition's overflow before compilation;
        // ordinary ranges retain the matcher's own bounded expansion semantics.
        for range in NUMERIC_RANGE.captures_iter(&pattern) {
            if let (Ok(start), Ok(end)) = (range[1].parse::<i64>(), range[2].parse::<i64>()) {
                if start.abs_diff(end).checked_add(1).is_none() {
                    return Err(format!(
                        "workspace package pattern '{pattern}' contains an overflowing numeric range"
                    ));
                }
            }
        }
        let glob = compile_matcher(
            &pattern,
            &CompileOptions {
                strict_brackets: true,
                nonegate: true,
                ..CompileOptions::default()
            },
        )
        .map_err(|error| format!("Invalid workspace package pattern '{pattern}': {error:?}"))?;
        if glob.is_match(candidate).map_err(|error| {
            format!("Cannot match workspace package pattern '{pattern}': {error:?}")
        })? {
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
    let pnpm = root.join("pnpm-workspace.yaml").exists();
    let marker = root.join(if pnpm {
        "pnpm-workspace.yaml"
    } else {
        "package.json"
    });
    let metadata = match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect '{}': {error}", marker.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Workspace control file '{}' must be a regular non-symlink file",
            marker.display()
        ));
    }
    let relative = app.strip_prefix(root).map_err(|_| {
        format!(
            "Application '{}' escapes workspace '{}'",
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
            "Workspace control file '{}' exceeds the 1 MiB limit",
            marker.display()
        ));
    }
    if pnpm {
        pnpm_workspace_contains(&contents, relative)
    } else {
        package_workspace_contains(&contents, relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_workspaces_support_both_shapes_and_preserve_exclusions() {
        for contents in [
            r#"{"workspaces":["apps/*","!apps/private"]}"#,
            r#"{"workspaces":{"packages":["apps/*","!apps/private"]}}"#,
        ] {
            assert!(package_workspace_contains(contents, Path::new("apps/api")).unwrap());
            for path in ["apps/private", "tools/api", "apps/nested/api"] {
                assert!(!package_workspace_contains(contents, Path::new(path)).unwrap());
            }
            assert!(package_workspace_contains(contents, Path::new("../apps/api")).is_err());
        }
        assert!(!package_workspace_contains("{}", Path::new("apps/api")).unwrap());
        for invalid in [
            r#"{"workspaces":false}"#,
            r#"{"workspaces":{"packages":"apps/*"}}"#,
        ] {
            assert!(package_workspace_contains(invalid, Path::new("apps/api")).is_err());
        }
        assert!(
            package_workspace_contains(r#"{"workspaces":[false]}"#, Path::new("apps/api")).is_err()
        );
    }

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
    fn membership_supports_bash_extglobs_and_nested_groups() {
        for (pattern, member, nonmember) in [
            ("apps/@(web|api)", "apps/web", "apps/mobile"),
            ("apps/?(web)api", "apps/api", "apps/webwebapi"),
            ("apps/+(web|api)", "apps/webapiweb", "apps/mobile"),
            ("apps/*(web|api)", "apps/apiweb", "apps/mobile"),
            ("apps/!(private|internal)", "apps/web", "apps/private"),
            ("apps/@(web|+(api|worker))", "apps/apiworker", "apps/mobile"),
            (
                "apps/!(private|@(internal|secret))",
                "apps/web",
                "apps/secret",
            ),
            ("!(private|internal)/*", "apps/web", "private/web"),
            (
                "{apps,packages}/@(web|api)",
                "packages/api",
                "packages/mobile",
            ),
        ] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            assert!(
                pnpm_workspace_contains(&contents, Path::new(member)).unwrap(),
                "{pattern} did not match {member}"
            );
            assert!(
                !pnpm_workspace_contains(&contents, Path::new(nonmember)).unwrap(),
                "{pattern} matched {nonmember}"
            );
        }
    }

    #[test]
    fn extglob_exclusions_remain_independent_of_order() {
        for patterns in [
            vec!["apps/*", "!apps/@(private|internal)"],
            vec!["!apps/@(private|internal)", "apps/*"],
            vec!["apps/*", "!apps/!(web|api)"],
        ] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": patterns})).unwrap();
            assert!(pnpm_workspace_contains(&contents, Path::new("apps/web")).unwrap());
            assert!(!pnpm_workspace_contains(&contents, Path::new("apps/private")).unwrap());
        }
    }

    #[test]
    fn hidden_names_require_explicit_pattern_components() {
        for (pattern, candidate, expected) in [
            ("apps/*", "apps/.private", false),
            ("apps/.*", "apps/.private", true),
            ("apps/.private", "apps/.private", true),
            ("**", ".apps/web", false),
            (".apps/*", ".apps/web", true),
            ("apps/@(.web|api)", "apps/.web", true),
        ] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            assert_eq!(
                pnpm_workspace_contains(&contents, Path::new(candidate)).unwrap(),
                expected,
                "{pattern}: {candidate}"
            );
        }
    }

    #[test]
    fn malformed_extglobs_error_and_incomplete_classes_remain_literal() {
        // Compile with strict groups so malformed configuration fails explicitly,
        // while valid nested groups retain the pnpm matcher dialect.
        for pattern in ["apps/@(web|api", "apps/+(web", "apps/!(private"] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            assert!(pnpm_workspace_contains(&contents, Path::new("apps/web")).is_err());
        }
        assert!(
            !pnpm_workspace_contains("packages: ['apps/[broken']", Path::new("apps/web")).unwrap()
        );
        assert!(
            pnpm_workspace_contains("packages: ['apps/[broken']", Path::new("apps/[broken"))
                .unwrap()
        );
    }

    #[test]
    fn pathological_workspace_patterns_fail_before_recursive_compilation() {
        let nested = format!("apps/{}web{}", "@(".repeat(129), ")".repeat(129));
        let oversized = format!("apps/{}", "w".repeat(64 * 1024));
        for pattern in [nested, oversized] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            let error = pnpm_workspace_contains(&contents, Path::new("apps/web")).unwrap_err();
            assert!(error.contains("compile limit"), "{error}");
        }
    }

    #[test]
    fn numeric_ranges_reject_overflow_and_preserve_bounded_ranges() {
        for pattern in [
            "apps/{-9223372036854775808..9223372036854775807}",
            "apps/{9223372036854775807..-9223372036854775808}",
            "apps/{-09223372036854775808..+09223372036854775807}",
        ] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            let error = pnpm_workspace_contains(&contents, Path::new("apps/web")).unwrap_err();
            assert!(error.contains("overflowing numeric range"), "{error}");
        }
        for (pattern, member, nonmember) in [
            ("apps/{1..3}", "apps/2", "apps/4"),
            ("apps/{+1..+3}", "apps/2", "apps/4"),
            (
                "apps/{-9223372036854775808..-9223372036854775806}",
                "apps/-9223372036854775807",
                "apps/-9223372036854775805",
            ),
            (
                "apps/{9223372036854775805..9223372036854775807}",
                "apps/9223372036854775806",
                "apps/9223372036854775804",
            ),
        ] {
            let contents =
                serde_yaml::to_string(&serde_json::json!({"packages": [pattern]})).unwrap();
            assert!(pnpm_workspace_contains(&contents, Path::new(member)).unwrap());
            assert!(!pnpm_workspace_contains(&contents, Path::new(nonmember)).unwrap());
        }
    }

    #[test]
    fn negative_group_budget_bounds_suffix_recompilation() {
        let repeated = format!("apps/{}web", "!(*).".repeat(9));
        let contents = serde_yaml::to_string(&serde_json::json!({"packages": [repeated]})).unwrap();
        let error = pnpm_workspace_contains(&contents, Path::new("apps/web")).unwrap_err();
        assert!(error.contains("8 negative-group compile limit"), "{error}");

        let nested = format!("apps/{}web{}", "!(".repeat(8), ")".repeat(8));
        let contents = serde_yaml::to_string(&serde_json::json!({"packages": [nested]})).unwrap();
        assert!(pnpm_workspace_contains(&contents, Path::new("apps/web")).is_ok());
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
        for contents in ["packages: [", "packages: false", "packages: ['apps/@(web']"] {
            assert!(pnpm_workspace_contains(contents, Path::new("apps/web")).is_err());
        }
    }
}
