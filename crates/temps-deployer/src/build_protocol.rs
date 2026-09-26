// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Versioned wire contract for building an image on an authenticated node.
//!
//! The request is `multipart/form-data`: a small JSON `spec` field followed by
//! a streamed `context` tar field. The response is newline-delimited JSON
//! [`BuildEvent`] values. This contract depends on neither replica placement
//! nor container execution, so a build-only agent can implement it unchanged.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

use crate::BuildResult;

pub const BUILD_PROTOCOL_VERSION: u16 = 1;
pub const MAX_BUILD_SPEC_BYTES: usize = 64 * 1024;
pub const MAX_BUILD_CONTEXT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_BUILD_CONTEXT_ENTRIES: usize = 100_000;
pub const MAX_BUILD_EVENT_BYTES: usize = 64 * 1024;

/// Paths are relative to the root of the tar archive. Build arguments are
/// intentionally absent from v1: the existing workflow cannot distinguish
/// ordinary arguments from values marked secret.
#[derive(Clone, Serialize, Deserialize)]
pub struct BuildSpec {
    pub version: u16,
    pub image_name: String,
    pub dockerfile: String,
    pub platform: Option<String>,
}

impl BuildSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != BUILD_PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported worker build protocol version {}; expected {}",
                self.version, BUILD_PROTOCOL_VERSION
            ));
        }
        if self.image_name.is_empty()
            || self.image_name.len() > 256
            || self.image_name.chars().any(char::is_whitespace)
        {
            return Err("Image name must be 1–256 characters without whitespace".to_string());
        }
        validate_archive_path(Path::new(&self.dockerfile))?;
        if self
            .platform
            .as_ref()
            .is_some_and(|platform| !crate::platform::is_buildable_platform(platform))
        {
            return Err("Worker build platform is not supported".to_string());
        }
        Ok(())
    }
}

/// Reject paths that could escape the extracted context on any supported OS.
pub fn validate_archive_path(path: &Path) -> Result<(), String> {
    let value = path.to_string_lossy();
    if value.is_empty()
        || value.contains('\\')
        || value.contains(':')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "Build context path '{value}' is not a safe relative path"
        ));
    }
    Ok(())
}

/// `.dockerignore` rules, evaluated the way the Docker CLI does before it
/// uploads a build context: patterns are relative to the context root, `*`
/// and `?` never cross a `/`, `**` spans directories, a pattern that matches
/// a directory excludes everything below it, `!` re-includes, and the last
/// matching rule wins.
///
/// Applied by the control plane before a source tree leaves it, so files the
/// project already keeps out of its image never cross to the build node.
pub struct DockerIgnore {
    rules: Vec<(globset::GlobMatcher, bool)>,
    has_exceptions: bool,
}

impl DockerIgnore {
    /// Parse `.dockerignore` contents. `source` names the file in errors.
    pub fn parse(contents: &str, source: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        let mut has_exceptions = false;
        for (index, raw) in contents.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (exclude, pattern) = match line.strip_prefix('!') {
                Some(rest) => (false, rest.trim()),
                None => (true, line),
            };
            let pattern = pattern.trim_start_matches('/');
            let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
            let pattern = pattern.trim_end_matches('/');
            if pattern.is_empty() || pattern == "." {
                continue;
            }
            let glob = globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(true)
                .build()
                .map_err(|error| {
                    format!(
                        "{source} line {}: invalid pattern '{pattern}': {error}",
                        index + 1
                    )
                })?;
            has_exceptions |= !exclude;
            rules.push((glob.compile_matcher(), exclude));
        }
        Ok(Self {
            rules,
            has_exceptions,
        })
    }

    /// An empty rule set: nothing is excluded.
    pub fn empty() -> Self {
        Self {
            rules: Vec::new(),
            has_exceptions: false,
        }
    }

    /// Whether `path` (relative to the context root) is excluded.
    pub fn is_excluded(&self, path: &Path) -> bool {
        let mut excluded = false;
        for (matcher, exclude) in &self.rules {
            let matches = path
                .ancestors()
                .filter(|candidate| !candidate.as_os_str().is_empty())
                .any(|candidate| matcher.is_match(candidate));
            if matches {
                excluded = *exclude;
            }
        }
        excluded
    }

    /// Whether any `!` rule exists. An excluded directory must then still be
    /// walked, because a later rule may re-include something inside it.
    pub fn has_exceptions(&self) -> bool {
        self.has_exceptions
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum BuildEvent {
    Log(String),
    Result(BuildResult),
    Failure(BuildFailure),
}

#[derive(Serialize, Deserialize)]
pub struct BuildFailure {
    pub kind: BuildFailureKind,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildFailureKind {
    Build,
    Timeout,
    Worker,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_spec_rejects_unsafe_paths_and_unknown_version() {
        let mut spec = BuildSpec {
            version: BUILD_PROTOCOL_VERSION,
            image_name: "app:latest".to_string(),
            dockerfile: "src/Dockerfile".to_string(),
            platform: Some("linux/amd64".to_string()),
        };
        assert!(spec.validate().is_ok());
        for path in ["../Dockerfile", "/etc/passwd", "a\\..\\b", "C:foo"] {
            spec.dockerfile = path.to_string();
            assert!(spec.validate().is_err(), "accepted {path}");
        }
        spec.dockerfile = "Dockerfile".to_string();
        spec.version += 1;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn dockerignore_follows_docker_cli_semantics() {
        let rules = DockerIgnore::parse(
            "# comment\n\n.env\nnode_modules\n/secrets/\n*.log\n**/*.pem\n!keep.log\n./dist\n",
            ".dockerignore",
        )
        .expect("valid rules");

        for excluded in [
            ".env",
            "node_modules/react/index.js",
            "secrets/api.key",
            "debug.log",
            "deep/nested/cert.pem",
            "cert.pem",
            "dist/app.js",
        ] {
            assert!(rules.is_excluded(Path::new(excluded)), "{excluded} kept");
        }
        for kept in [
            "src/.env",
            "packages/app/node_modules/x.js",
            "logs/debug.log",
            "keep.log",
            "Dockerfile",
            "src/main.rs",
        ] {
            assert!(!rules.is_excluded(Path::new(kept)), "{kept} excluded");
        }
        assert!(rules.has_exceptions());
    }

    #[test]
    fn dockerignore_last_matching_rule_wins() {
        let rules = DockerIgnore::parse("docs\n!docs/README.md\ndocs/README.md\n", ".dockerignore")
            .expect("valid rules");
        assert!(rules.is_excluded(Path::new("docs/README.md")));

        let rules = DockerIgnore::parse("*\n!src\n", ".dockerignore").expect("valid rules");
        assert!(rules.is_excluded(Path::new("package.json")));
        assert!(!rules.is_excluded(Path::new("src/main.ts")));
    }

    #[test]
    fn dockerignore_reports_the_offending_line() {
        let error = DockerIgnore::parse("ok\n[unclosed\n", "app/.dockerignore")
            .err()
            .expect("invalid glob");
        assert!(error.contains("app/.dockerignore line 2"), "{error}");
    }

    #[test]
    fn build_event_round_trips() {
        let event = BuildEvent::Failure(BuildFailure {
            kind: BuildFailureKind::Build,
            message: "Dockerfile failed".to_string(),
        });
        let encoded = serde_json::to_vec(&event).expect("serialize event");
        assert!(matches!(
            serde_json::from_slice::<BuildEvent>(&encoded).expect("deserialize event"),
            BuildEvent::Failure(_)
        ));
    }
}
