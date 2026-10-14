// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Versioned wire contract for building an image on an authenticated node.
//!
//! The request is `multipart/form-data`: a small JSON `spec` field followed by
//! a streamed `context` tar field. The response is newline-delimited JSON
//! [`BuildEvent`] values. This contract depends on neither replica placement
//! nor container execution, so a build-only agent can implement it unchanged.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use crate::BuildResult;

// Internal opt-in for generated Dockerfiles whose app context was widened.
// Archive builders evaluate root exclusions independently of app-specific rules;
// ordinary custom Dockerfiles retain Docker's specific-file precedence.
pub const WORKSPACE_ROOT_IGNORE_MARKER: &str = "# temps: preserve-workspace-root-ignore";

/// The original protocol, which carries no build-argument values. A control
/// plane still sends it for a build without any, so a worker that has not
/// been upgraded keeps receiving those builds.
pub const BUILD_PROTOCOL_VERSION: u16 = 1;
/// Adds [`BuildSpec::build_args`]. Sent only when a build has values, so an
/// agent that predates it refuses the build (unknown version) instead of
/// silently building without them.
pub const BUILD_PROTOCOL_VERSION_BUILD_ARGS: u16 = 2;
pub const MAX_BUILD_SPEC_BYTES: usize = 256 * 1024;
pub const MAX_BUILD_CONTEXT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_BUILD_CONTEXT_ENTRIES: usize = 100_000;
pub const MAX_BUILD_EVENT_BYTES: usize = 64 * 1024;
/// Most build arguments one worker build may receive.
pub const MAX_BUILD_ARGS: usize = 512;
/// Longest build-argument name a worker accepts.
pub const MAX_BUILD_ARG_NAME_LEN: usize = 256;
/// Largest single build-argument value a worker accepts.
pub const MAX_BUILD_ARG_VALUE_BYTES: usize = 32 * 1024;
/// Largest total of build-argument names and values in one spec. Leaves room
/// for the rest of the spec (and JSON escaping) under [`MAX_BUILD_SPEC_BYTES`].
pub const MAX_BUILD_ARGS_BYTES: usize = 128 * 1024;

/// Paths are relative to the root of the tar archive.
///
/// Deliberately not `Debug`: [`BuildSpec::build_args`] holds variable values,
/// and a stray `{:?}` must not be able to put them in a log.
#[derive(Clone, Serialize, Deserialize)]
pub struct BuildSpec {
    pub version: u16,
    pub image_name: String,
    pub dockerfile: String,
    pub platform: Option<String>,
    /// BuildKit's predefined `BUILDKIT_CACHE_MOUNT_NS`: the control plane's
    /// per-project/environment/ref namespace for `RUN --mount=type=cache`.
    /// Not a project variable and not a credential, so it is the one build
    /// argument a worker receives. Without it, every project built on the
    /// same worker shares writable cache mounts. Optional on the wire: an
    /// agent that predates it ignores the field and builds as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_namespace: Option<String>,
    /// Values for the Dockerfile's `ARG` instructions, applied exactly as
    /// `docker build --build-arg` would be (protocol v2). The control plane
    /// decides which values may leave it -- never one marked secret or a
    /// platform credential -- and sends only names the Dockerfile declares.
    /// They travel inside this spec over the node's authenticated build
    /// channel and are held in memory on both sides; neither side writes
    /// them to disk or to a log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub build_args: BTreeMap<String, String>,
}

/// BuildKit's predefined build argument that prefixes every cache mount ID.
pub const CACHE_MOUNT_NAMESPACE_ARG: &str = "BUILDKIT_CACHE_MOUNT_NS";

/// Longest cache namespace a worker accepts; the control plane sends 64 hex
/// characters.
pub const MAX_CACHE_NAMESPACE_LEN: usize = 128;

/// Whether `namespace` is safe to hand to BuildKit as a cache mount ID prefix.
pub fn is_valid_cache_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= MAX_CACHE_NAMESPACE_LEN
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Build arguments Docker accepts without an `ARG` declaration.
const PREDEFINED_BUILD_ARGS: &[&str] = &[
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "FTP_PROXY",
    "ftp_proxy",
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Whether `name` can be a Dockerfile `ARG` a worker build may receive: a
/// plain identifier, and not one of BuildKit's own `BUILDKIT_*` arguments,
/// which change how the worker builds (frontend image, cache export) rather
/// than what the Dockerfile reads. The cache namespace has its own field.
pub fn is_worker_build_arg_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    starts_well
        && name.len() <= MAX_BUILD_ARG_NAME_LEN
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.to_ascii_uppercase().starts_with("BUILDKIT_")
}

/// Whether a build argument named `name` is read by a Dockerfile that
/// declares `declared` -- through an `ARG`, or because Docker predefines it.
pub fn dockerfile_reads_build_arg(declared: &BTreeSet<String>, name: &str) -> bool {
    declared.contains(name) || PREDEFINED_BUILD_ARGS.contains(&name)
}

impl BuildSpec {
    /// The protocol version a spec carrying `build_args` must declare.
    pub fn version_for(build_args: &BTreeMap<String, String>) -> u16 {
        if build_args.is_empty() {
            BUILD_PROTOCOL_VERSION
        } else {
            BUILD_PROTOCOL_VERSION_BUILD_ARGS
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != BUILD_PROTOCOL_VERSION
            && self.version != BUILD_PROTOCOL_VERSION_BUILD_ARGS
        {
            return Err(format!(
                "Unsupported worker build protocol version {}; this worker supports {} and {}",
                self.version, BUILD_PROTOCOL_VERSION, BUILD_PROTOCOL_VERSION_BUILD_ARGS
            ));
        }
        self.validate_build_args()?;
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
        if self
            .cache_namespace
            .as_deref()
            .is_some_and(|namespace| !is_valid_cache_namespace(namespace))
        {
            return Err(format!(
                "Worker build cache namespace must be 1-{MAX_CACHE_NAMESPACE_LEN} ASCII letters, \
                 digits, '-', '_' or '.'"
            ));
        }
        Ok(())
    }

    /// Names and sizes only: no message may quote a value.
    fn validate_build_args(&self) -> Result<(), String> {
        if self.build_args.is_empty() {
            return Ok(());
        }
        if self.version < BUILD_PROTOCOL_VERSION_BUILD_ARGS {
            return Err(format!(
                "Worker build protocol version {} carries no build arguments; version {} is \
                 required to send them",
                self.version, BUILD_PROTOCOL_VERSION_BUILD_ARGS
            ));
        }
        if self.build_args.len() > MAX_BUILD_ARGS {
            return Err(format!(
                "Worker builds accept at most {MAX_BUILD_ARGS} build arguments, got {}",
                self.build_args.len()
            ));
        }
        let mut total = 0_usize;
        for (name, value) in &self.build_args {
            if !is_worker_build_arg_name(name) {
                return Err(format!(
                    "Worker build argument '{}' is not a valid ARG name (letters, digits and \
                     '_', not starting with a digit, at most {MAX_BUILD_ARG_NAME_LEN} \
                     characters, and not a BUILDKIT_* argument)",
                    name.escape_debug()
                ));
            }
            if value.len() > MAX_BUILD_ARG_VALUE_BYTES {
                return Err(format!(
                    "Worker build argument '{name}' exceeds {MAX_BUILD_ARG_VALUE_BYTES} bytes"
                ));
            }
            if value.contains('\0') {
                return Err(format!(
                    "Worker build argument '{name}' contains a NUL byte"
                ));
            }
            total = total.saturating_add(name.len() + value.len());
        }
        if total > MAX_BUILD_ARGS_BYTES {
            return Err(format!(
                "Worker build arguments total {total} bytes, more than the \
                 {MAX_BUILD_ARGS_BYTES}-byte limit"
            ));
        }
        Ok(())
    }
}

/// The names a Dockerfile declares with `ARG` (or `ONBUILD ARG`), or `None`
/// when a declaration names a variable reference and cannot be read.
///
/// Instructions continued with the escape character (`\`, or the one set by
/// a leading `# escape=` directive) are joined first, exactly as Docker joins
/// them: the lines are concatenated with nothing in between, and comment and
/// empty lines inside a continuation are skipped. So `ARG API_\` followed by
/// `URL` declares `API_URL`. Every other physical line is treated as a
/// possible instruction, so a heredoc line that happens to start with `ARG`
/// only adds a name: callers use the result to decide which values a build
/// reads, where an extra name is harmless and a missing one is not.
pub fn dockerfile_build_arg_names(contents: &str) -> Option<BTreeSet<String>> {
    let contents = contents.trim_start_matches('\u{feff}');
    let escape = dockerfile_escape_character(contents);
    let mut names = BTreeSet::new();
    let mut lines = contents.lines();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let mut instruction = String::new();
        let mut current = line;
        while let Some(continued) = strip_line_continuation(current, escape) {
            instruction.push_str(continued);
            // Docker drops comment and empty lines inside a continuation.
            let next = lines
                .by_ref()
                .find(|next| !next.trim().is_empty() && !next.trim_start().starts_with('#'));
            current = next.unwrap_or_default();
        }
        instruction.push_str(current);
        let mut words = instruction.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let declares = keyword.eq_ignore_ascii_case("ARG")
            || (keyword.eq_ignore_ascii_case("ONBUILD")
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("ARG")));
        if !declares {
            continue;
        }
        let mut declared_any = false;
        for word in words {
            // `NAME=default`; a quoted default containing spaces only adds
            // spurious names, never hides one.
            let name = word.split('=').next().unwrap_or_default();
            if name.is_empty() || name.contains('$') {
                return None;
            }
            names.insert(name.to_string());
            declared_any = true;
        }
        if !declared_any {
            return None;
        }
    }
    Some(names)
}

/// `line` without its line continuation, or `None` when it does not end with
/// one. Docker's rule: the escape character, preceded by any character other
/// than the escape character, with only blanks after it. An escaped escape
/// (`\\`) at the end of a line therefore does not continue it.
fn strip_line_continuation(line: &str, escape: char) -> Option<&str> {
    let body = line.trim_end_matches([' ', '\t']).strip_suffix(escape)?;
    match body.chars().next_back() {
        Some(previous) if previous != escape => Some(body),
        _ => None,
    }
}

/// The escape character set by a `# escape=` parser directive, which Docker
/// only honours among the comment lines at the very top of the file.
fn dockerfile_escape_character(contents: &str) -> char {
    for line in contents.lines() {
        let Some(directive) = line.trim().strip_prefix('#') else {
            break;
        };
        let Some((key, value)) = directive.split_once('=') else {
            break;
        };
        if key.trim().eq_ignore_ascii_case("escape") {
            if let Some(character @ ('\\' | '`')) = value.trim().chars().next() {
                return character;
            }
        }
    }
    '\\'
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
            cache_namespace: None,
            build_args: BTreeMap::new(),
        };
        assert!(spec.validate().is_ok());
        for path in ["../Dockerfile", "/etc/passwd", "a\\..\\b", "C:foo"] {
            spec.dockerfile = path.to_string();
            assert!(spec.validate().is_err(), "accepted {path}");
        }
        spec.dockerfile = "Dockerfile".to_string();
        spec.version = BUILD_PROTOCOL_VERSION_BUILD_ARGS;
        assert!(spec.validate().is_ok());
        spec.version += 1;
        assert!(spec.validate().is_err());
        spec.version = 0;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn build_spec_build_args_need_v2_and_are_bounded() {
        let mut spec = BuildSpec {
            version: BUILD_PROTOCOL_VERSION,
            image_name: "app:latest".to_string(),
            dockerfile: "Dockerfile".to_string(),
            platform: None,
            cache_namespace: None,
            build_args: BTreeMap::from([(
                "VITE_API_URL".to_string(),
                "https://api.example.test".to_string(),
            )]),
        };
        // v1 has no field for values: a v1 spec carrying them is malformed.
        assert!(spec.validate().is_err());
        spec.version = BuildSpec::version_for(&spec.build_args);
        assert_eq!(spec.version, BUILD_PROTOCOL_VERSION_BUILD_ARGS);
        assert!(spec.validate().is_ok());
        assert_eq!(
            BuildSpec::version_for(&BTreeMap::new()),
            BUILD_PROTOCOL_VERSION
        );

        let secret = "synthetic-value-never-quoted";
        for name in [
            "",
            "1LEADING_DIGIT",
            "has-dash",
            "has space",
            "BUILDKIT_SYNTAX",
            "buildkit_inline_cache",
        ] {
            let mut bad = spec.clone();
            bad.build_args = BTreeMap::from([(name.to_string(), secret.to_string())]);
            let error = bad.validate().expect_err(name);
            assert!(!error.contains(secret), "{error}");
        }
        let mut long_name = spec.clone();
        long_name.build_args =
            BTreeMap::from([("A".repeat(MAX_BUILD_ARG_NAME_LEN + 1), "x".to_string())]);
        assert!(long_name.validate().is_err());
        let mut large = spec.clone();
        large.build_args =
            BTreeMap::from([("BIG".to_string(), "x".repeat(MAX_BUILD_ARG_VALUE_BYTES + 1))]);
        let error = large.validate().expect_err("oversized value");
        assert!(error.contains("BIG"), "{error}");
        let mut nul = spec.clone();
        nul.build_args = BTreeMap::from([("NUL".to_string(), "a\0b".to_string())]);
        assert!(nul.validate().is_err());
        let mut many = spec.clone();
        many.build_args = (0..=MAX_BUILD_ARGS)
            .map(|index| (format!("A{index}"), String::new()))
            .collect();
        assert!(many.validate().is_err());
        let mut total = spec.clone();
        total.build_args = (0..8)
            .map(|index| (format!("V{index}"), "x".repeat(MAX_BUILD_ARG_VALUE_BYTES)))
            .collect();
        assert!(total.validate().is_err());

        // An agent that predates v2 sees no build arguments at all in a v1
        // spec, and a v2 spec round-trips its values.
        let wire = serde_json::to_string(&spec).expect("serialize");
        let parsed: BuildSpec = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(parsed.build_args, spec.build_args);
        let legacy: BuildSpec = serde_json::from_str(
            r#"{"version":1,"image_name":"app:latest","dockerfile":"Dockerfile","platform":null}"#,
        )
        .expect("v1 spec");
        assert!(legacy.build_args.is_empty());
    }

    #[test]
    fn worker_build_arg_names_exclude_buildkit_arguments() {
        for name in ["VITE_API_URL", "_x", "NEXT_PUBLIC_A1", "lower"] {
            assert!(is_worker_build_arg_name(name), "{name}");
        }
        for name in [
            "",
            "9A",
            "A-B",
            "A B",
            "BUILDKIT_CACHE_MOUNT_NS",
            "BuildKit_X",
        ] {
            assert!(!is_worker_build_arg_name(name), "{name}");
        }
        let declared = BTreeSet::from(["API_URL".to_string()]);
        assert!(dockerfile_reads_build_arg(&declared, "API_URL"));
        assert!(dockerfile_reads_build_arg(&declared, "HTTPS_PROXY"));
        assert!(!dockerfile_reads_build_arg(&declared, "api_url"));
    }

    #[test]
    fn dockerfile_build_arg_names_follows_docker_line_continuations() {
        let names = |contents: &str| {
            dockerfile_build_arg_names(contents).map(|names| names.into_iter().collect::<Vec<_>>())
        };
        assert_eq!(names("FROM scratch\n"), Some(vec![]));
        assert_eq!(
            names("\u{feff}ARG A=1 B\n  arg\tC=\"x y\"\n# ARG COMMENTED\nRUN echo ARG\n"),
            Some(vec!["A".into(), "B".into(), "C".into(), "y\"".into()])
        );
        assert_eq!(
            names("FROM base\nONBUILD ARG TOKEN\n"),
            Some(vec!["TOKEN".into()])
        );
        // A continued declaration names every line it spans, skipping the
        // comment and empty lines Docker drops inside a continuation.
        assert_eq!(
            names("ARG A \\\n# note\n\n  B=2 \\\n  C\nRUN true\n"),
            Some(vec!["A".into(), "B".into(), "C".into()])
        );
        // The escape directive changes the continuation character.
        assert_eq!(
            names("# escape=`\nARG A `\n  B\nRUN dir c:\\\n"),
            Some(vec!["A".into(), "B".into()])
        );
        // A continued RUN line that happens to start with ARG is not one.
        assert_eq!(names("RUN echo \\\n  ARG HIDDEN\n"), Some(vec![]));
        // Docker joins continued lines with nothing in between, so a name
        // split across lines is one name, and blanks after the escape
        // character are ignored.
        assert_eq!(
            names("ARG API_\\\nURL=1 \\  \nBUILD_\\\n# note\nLABEL\nRUN true\n"),
            Some(vec!["API_URL".into(), "BUILD_LABEL".into()])
        );
        // An escaped escape at the end of a line does not continue it, so
        // the next line is still its own instruction.
        assert_eq!(
            names("RUN echo a\\\\\nARG NEXT\n"),
            Some(vec!["NEXT".into()])
        );
        // A continuation that runs into the end of the file.
        assert_eq!(names("ARG LAST \\\n"), Some(vec!["LAST".into()]));
        // A referenced name cannot be resolved here.
        assert_eq!(names("ARG ${NAME}\n"), None);
        assert_eq!(names("ARG\n"), None);
    }

    #[test]
    fn build_spec_cache_namespace_is_optional_and_constrained() {
        // An older control plane sends no namespace; an older agent ignores it.
        let legacy: BuildSpec = serde_json::from_str(
            r#"{"version":1,"image_name":"app:latest","dockerfile":"Dockerfile","platform":null}"#,
        )
        .expect("spec without a cache namespace");
        assert!(legacy.cache_namespace.is_none());
        assert!(legacy.validate().is_ok());
        assert!(!serde_json::to_string(&legacy)
            .expect("serialize")
            .contains("cache_namespace"));

        let mut spec = legacy;
        spec.cache_namespace = Some("a".repeat(64));
        assert!(spec.validate().is_ok());
        for bad in [
            String::new(),
            "a".repeat(MAX_CACHE_NAMESPACE_LEN + 1),
            "ns with space".to_string(),
            "ns/../x".to_string(),
            "ns,other=1".to_string(),
        ] {
            spec.cache_namespace = Some(bad.clone());
            assert!(spec.validate().is_err(), "accepted {bad:?}");
        }
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
