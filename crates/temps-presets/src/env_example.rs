// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Detection and parsing of `.env.example`-style scaffolding files.
//!
//! Many repositories ship a template listing every environment variable the
//! app expects (with placeholder/default values and often a `# comment`
//! documenting each one) without committing the real `.env`. Surfacing that
//! during project creation lets the New Project wizard show the user exactly
//! what to fill in instead of requiring them to already know their app's env
//! vars or dig through the repo themselves.

/// Filenames recognized as ".env.example"-style scaffolding, most common
/// first. Matched against the file's base name only (any directory).
pub const ENV_EXAMPLE_FILE_NAMES: &[&str] = &[
    ".env.example",
    ".env.sample",
    ".env.dist",
    ".env.template",
    ".env.defaults",
    "env.example",
    "env.sample",
    "example.env",
    "sample.env",
];

/// Finds env-example files in a repository's file tree (as produced by the
/// existing preset-detection tree fetch — no extra API calls). Shallower
/// paths sort first, so a root-level file is preferred over one buried in a
/// monorepo subpackage when the caller only wants a single "best" match.
pub fn detect_env_example_files(files: &[String]) -> Vec<String> {
    let mut matches: Vec<String> = files
        .iter()
        .filter(|path| {
            let filename = path.rsplit('/').next().unwrap_or(path.as_str());
            ENV_EXAMPLE_FILE_NAMES.contains(&filename)
        })
        .cloned()
        .collect();

    matches.sort_by_key(|path| (path.matches('/').count(), path.clone()));
    matches
}

/// Finds env-example files directly inside `root_directory`.
///
/// Repository trees use paths relative to the repository root, while project
/// root directories are commonly written with a leading `./`. A repository
/// root of `.`, `./`, or an empty string therefore matches only root-level
/// files; it does not fall through to env files belonging to nested apps.
pub fn detect_env_example_files_in_directory(
    files: &[String],
    root_directory: &str,
) -> Vec<String> {
    let normalized_root = root_directory
        .trim()
        .strip_prefix("./")
        .unwrap_or(root_directory.trim())
        .trim_end_matches('/');
    let normalized_root = if normalized_root == "." {
        ""
    } else {
        normalized_root
    };

    detect_env_example_files(files)
        .into_iter()
        .filter(|path| {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            parent == normalized_root
        })
        .collect()
}

/// A single variable parsed out of an env-example file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvExampleVariable {
    pub key: String,
    /// The placeholder/default value as written in the file (may be empty).
    pub default_value: String,
    /// `# comment` line(s) immediately preceding this key, if any.
    pub description: Option<String>,
}

/// Parses `.env.example`-style content into key/default-value/description
/// entries.
///
/// Convention followed: one or more consecutive `# comment` lines
/// immediately above a `KEY=value` line are treated as that variable's
/// description. A blank line breaks the association, so a comment separated
/// from the next key by whitespace is treated as unrelated (e.g. a section
/// header) rather than misattributed to it.
pub fn parse_env_example(content: &str) -> Vec<EnvExampleVariable> {
    let mut variables = Vec::new();
    let mut pending_comment_lines: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line.is_empty() {
            pending_comment_lines.clear();
            continue;
        }

        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if !comment.is_empty() {
                pending_comment_lines.push(comment.to_string());
            }
            continue;
        }

        // `export KEY=value` is a common shell-sourceable variant.
        let line = line.strip_prefix("export ").unwrap_or(line);

        let Some((key, value)) = line.split_once('=') else {
            pending_comment_lines.clear();
            continue;
        };

        let key = key.trim();
        if !is_valid_env_key(key) {
            pending_comment_lines.clear();
            continue;
        }

        let description = if pending_comment_lines.is_empty() {
            None
        } else {
            Some(pending_comment_lines.join(" "))
        };

        variables.push(EnvExampleVariable {
            key: key.to_string(),
            default_value: unquote(value.trim()),
            description,
        });
        pending_comment_lines.clear();
    }

    variables
}

fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_canonical_env_example_at_root() {
        let files = vec!["package.json".to_string(), ".env.example".to_string()];
        assert_eq!(detect_env_example_files(&files), vec![".env.example"]);
    }

    #[test]
    fn detects_common_variants() {
        let files = vec![
            ".env.sample".to_string(),
            ".env.dist".to_string(),
            ".env.template".to_string(),
            "env.example".to_string(),
            "example.env".to_string(),
        ];
        let detected = detect_env_example_files(&files);
        assert_eq!(detected.len(), files.len());
    }

    #[test]
    fn ignores_a_real_env_file() {
        let files = vec![".env".to_string(), ".env.local".to_string()];
        assert!(detect_env_example_files(&files).is_empty());
    }

    #[test]
    fn prefers_shallower_paths() {
        let files = vec![
            "apps/api/.env.example".to_string(),
            ".env.example".to_string(),
        ];
        let detected = detect_env_example_files(&files);
        assert_eq!(detected[0], ".env.example");
        assert_eq!(detected[1], "apps/api/.env.example");
    }

    #[test]
    fn scopes_detection_to_selected_project_directory() {
        let files = vec![
            ".env.example".to_string(),
            "apps/api/.env.sample".to_string(),
            "apps/web/.env.example".to_string(),
            "apps/web/nested/.env.example".to_string(),
        ];

        assert_eq!(
            detect_env_example_files_in_directory(&files, "./apps/web"),
            vec!["apps/web/.env.example"]
        );
        assert_eq!(
            detect_env_example_files_in_directory(&files, "./"),
            vec![".env.example"]
        );
    }

    #[test]
    fn selected_directory_without_env_example_does_not_fall_back() {
        let files = vec![
            ".env.example".to_string(),
            "apps/api/.env.example".to_string(),
        ];

        assert!(detect_env_example_files_in_directory(&files, "apps/web").is_empty());
    }

    #[test]
    fn parses_key_value_pairs() {
        let content = "DATABASE_URL=postgres://localhost/db\nPORT=3000\n";
        let vars = parse_env_example(content);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].key, "DATABASE_URL");
        assert_eq!(vars[0].default_value, "postgres://localhost/db");
        assert_eq!(vars[0].description, None);
        assert_eq!(vars[1].key, "PORT");
        assert_eq!(vars[1].default_value, "3000");
    }

    #[test]
    fn attaches_preceding_comment_as_description() {
        let content = "# Postgres connection string\nDATABASE_URL=\n";
        let vars = parse_env_example(content);
        assert_eq!(vars.len(), 1);
        assert_eq!(
            vars[0].description.as_deref(),
            Some("Postgres connection string")
        );
        assert_eq!(vars[0].default_value, "");
    }

    #[test]
    fn joins_multiple_consecutive_comment_lines() {
        let content = "# Stripe secret key.\n# Get one at dashboard.stripe.com/apikeys\nSTRIPE_SECRET_KEY=sk_test_\n";
        let vars = parse_env_example(content);
        assert_eq!(
            vars[0].description.as_deref(),
            Some("Stripe secret key. Get one at dashboard.stripe.com/apikeys")
        );
    }

    #[test]
    fn blank_line_breaks_comment_association() {
        let content = "# Section: Database\n\nDATABASE_URL=\n";
        let vars = parse_env_example(content);
        assert_eq!(vars[0].description, None);
    }

    #[test]
    fn strips_surrounding_quotes() {
        let content = "SECRET=\"abc123\"\nOTHER='xyz'\n";
        let vars = parse_env_example(content);
        assert_eq!(vars[0].default_value, "abc123");
        assert_eq!(vars[1].default_value, "xyz");
    }

    #[test]
    fn handles_export_prefix() {
        let content = "export NODE_ENV=production\n";
        let vars = parse_env_example(content);
        assert_eq!(vars[0].key, "NODE_ENV");
        assert_eq!(vars[0].default_value, "production");
    }

    #[test]
    fn ignores_malformed_lines() {
        let content = "not a valid line\nDATABASE_URL=postgres://x\n123INVALID=nope\n";
        let vars = parse_env_example(content);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "DATABASE_URL");
    }

    #[test]
    fn empty_content_yields_no_variables() {
        assert!(parse_env_example("").is_empty());
    }
}
