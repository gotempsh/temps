// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Redacting secret values from Compose output and classifying
//! `compose config` failures.

use regex::Regex;
use std::sync::LazyLock;

use super::exec::MAX_STREAMED_LINE_BYTES;
use super::support::{COMPOSE_CONFIG_FLAGS, MIN_COMPOSE_VERSION};
use super::{ComposeConfigFailureClass, ComposeDeployRequest, MAX_COMPOSE_DIAGNOSTIC_BYTES};

/// True when a container's log tail shows its entrypoint being denied a
/// privileged startup operation — the signature of Temps' own `cap_drop: ALL`
/// sandbox rather than a fault in the image or the user's configuration.
///
/// Official entrypoints (postgres/mysql/mariadb/mongo, nginx, and many
/// others, e.g. Gitea) start as root, `chown`/`chmod` a data/cache
/// directory, then drop to a service user via `gosu`/`su-exec`. Every
/// sandboxed service is granted `ComposeExecutor::RELAXED_CAPABILITIES` by
/// default specifically so those steps succeed, so a denial matching this
/// pattern now means the image needs something outside that default set
/// (or hit an unrelated permission problem, e.g. a read-only mount) —
/// see `capability_denial_remediation`.
///
/// Deliberately a two-factor match: "operation not permitted" on its own
/// appears in plenty of ordinary application errors, and pointing those at a
/// capability toggle that cannot help would be worse than saying nothing.
/// Requiring a privileged-operation verb alongside it keeps the hint tied to
/// the failure mode it actually explains.
pub(super) fn looks_like_capability_denial(logs: &str) -> bool {
    const PRIVILEGED_OPERATIONS: [&str; 8] = [
        "chmod",
        "chown",
        "setgroups",
        "setuid",
        "setgid",
        "failed switching to",
        "su-exec",
        "gosu",
    ];

    let lower = logs.to_ascii_lowercase();
    lower.contains("operation not permitted")
        && PRIVILEGED_OPERATIONS
            .iter()
            .any(|operation| lower.contains(operation))
}

/// Render service names as a quoted, comma-separated list so a remediation
/// message reads the same for one service as for several.
pub(super) fn quote_service_list(services: &[&String]) -> String {
    services
        .iter()
        .map(|service| format!("'{service}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every literal value a Compose deployment knows to be sensitive, gathered
/// once so it can be scrubbed out of any diagnostic the deploy produces.
///
/// Takes values rather than a map because the sources overlap by key —
/// a secret and an environment variable may share a name, and merging them
/// into one map would silently drop one of the two values from redaction.
pub(super) fn collect_redactable_values(request: &ComposeDeployRequest) -> Vec<String> {
    let mut values = request
        .environment_vars
        .values()
        .chain(request.build_args.values())
        .chain(request.secrets.values())
        .filter(|value| !value.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(env_content) = request.env_content.as_deref() {
        values.extend(env_content.lines().filter_map(|line| {
            let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (_, value) = line.split_once('=')?;
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            (!value.is_empty()).then(|| value.to_string())
        }));
    }
    // Streamed output is redacted one line at a time, where a multi-line
    // value (a PEM key, a JSON credential) never matches as a whole. Its
    // lines are redacted individually; short ones such as `}` are skipped
    // because they would blank unrelated output without protecting anything.
    let value_lines = values
        .iter()
        .filter(|value| value.contains('\n'))
        .flat_map(|value| value.lines())
        .map(str::trim)
        .filter(|line| line.len() >= MIN_REDACTED_LINE_BYTES)
        .map(str::to_string)
        .collect::<Vec<_>>();
    values.extend(value_lines);
    values
}

/// Shortest line of a multi-line sensitive value that is redacted on its own.
const MIN_REDACTED_LINE_BYTES: usize = 8;

pub(super) fn sanitize_compose_diagnostic(diagnostic: &str, redact_values: &[String]) -> String {
    let mut sanitized = redact_compose_text(diagnostic, redact_values);

    // Keep the tail: Compose and BuildKit print the failing step and its
    // error last, after everything that succeeded.
    if sanitized.len() > MAX_COMPOSE_DIAGNOSTIC_BYTES {
        let mut boundary = sanitized.len() - MAX_COMPOSE_DIAGNOSTIC_BYTES;
        while !sanitized.is_char_boundary(boundary) {
            boundary += 1;
        }
        sanitized = format!(
            "… earlier output truncated; the full output is in the deployment log\n{}",
            &sanitized[boundary..]
        );
    }
    sanitized
}

/// Remove the start of a stderr capture whose front was trimmed away.
///
/// The trim can cut through a sensitive value, leaving its suffix at the very
/// start, where no rule recognises it. If a whole line follows, the capture
/// starts there. Otherwise the capture is one line longer than the limit,
/// whose ending is the error and must survive: it is masked with that leading
/// fragment included (see [`mask_cut_text`]), then loses only the token the
/// cut went through.
pub(super) fn drop_leading_fragment(captured: &mut Vec<u8>, redact_values: &[String]) {
    if let Some(newline) = captured.iter().position(|byte| *byte == b'\n') {
        if captured[newline + 1..]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace())
        {
            captured.drain(..=newline);
            return;
        }
    }
    let masked = mask_cut_text(captured, redact_values, CutEdge::Start);
    *captured = format!("… {masked}").into_bytes();
}

/// Which end of a text a cut went through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CutEdge {
    Start,
    End,
}

/// Mask `raw`, which was cut at `edge`, then drop the token the cut went
/// through.
///
/// Everything is located on the original bytes before anything is decoded or
/// removed: every complete occurrence of a sensitive value, plus the fragment
/// the cut left of a split value (the longest proper suffix of a value that
/// the bytes start with, or the longest proper prefix they end with). A cut
/// inside a multibyte character would turn into U+FFFD on decoding and stop
/// matching, so matching never happens on decoded text. The union of those
/// ranges is masked in one pass, so neither masking nor cutting can break a
/// value into unrecognised pieces, including a value containing spaces. Only
/// then does the cut edge move to the nearest whitespace, which removes a
/// split credential-shaped token (a URL whose userinfo lost its `@`); masked
/// values contain no whitespace, so this cannot split one. Text with no
/// whitespace keeps everything.
fn mask_cut_text(raw: &[u8], redact_values: &[String], edge: CutEdge) -> String {
    let fragment = redact_values
        .iter()
        .map(|value| match edge {
            CutEdge::Start => value_suffix_prefix_overlap(value.as_bytes(), raw),
            CutEdge::End => {
                let reversed_value: Vec<u8> = value.bytes().rev().collect();
                let reversed_raw: Vec<u8> = raw.iter().rev().copied().collect();
                value_suffix_prefix_overlap(&reversed_value, &reversed_raw)
            }
        })
        .max()
        .unwrap_or(0);
    let fragment_range = match edge {
        CutEdge::Start => 0..fragment,
        CutEdge::End => raw.len() - fragment..raw.len(),
    };
    let masked = redact_byte_ranges(raw, redact_values, Some(fragment_range));
    match edge {
        CutEdge::Start => match masked.find(char::is_whitespace) {
            Some(space) => masked[space..].trim_start().to_string(),
            None => masked,
        },
        CutEdge::End => match masked.rfind(char::is_whitespace) {
            Some(space) => masked[..space].trim_end().to_string(),
            None => masked,
        },
    }
}

/// Length of the longest proper suffix of `value` that `text` starts with.
/// Applied to reversed bytes, the longest proper prefix of `value` that a
/// text ends with.
///
/// Linear in `value`'s length: a KMP prefix function over the candidate
/// prefix of `text`, then one pass over `value`, ending in the state that
/// says how much of that prefix matches the end of `value`.
fn value_suffix_prefix_overlap(value: &[u8], text: &[u8]) -> usize {
    let pattern = &text[..text.len().min(value.len().saturating_sub(1))];
    if pattern.is_empty() {
        return 0;
    }
    let prefix_function = kmp_prefix_function(pattern);
    let mut matched = 0;
    for &byte in value {
        while matched > 0 && (matched == pattern.len() || byte != pattern[matched]) {
            matched = prefix_function[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
        }
    }
    matched
}

/// KMP prefix function: for each position, the length of the longest proper
/// prefix of `pattern[..=position]` that is also its suffix.
fn kmp_prefix_function(pattern: &[u8]) -> Vec<usize> {
    let mut prefix_function = vec![0_usize; pattern.len()];
    let mut matched = 0;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix_function[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix_function[index] = matched;
    }
    prefix_function
}

/// Byte ranges of every non-overlapping occurrence of `needle` in
/// `haystack`, in linear time.
fn find_occurrences(haystack: &[u8], needle: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return found;
    }
    let prefix_function = kmp_prefix_function(needle);
    let mut matched = 0;
    for (index, &byte) in haystack.iter().enumerate() {
        while matched > 0 && byte != needle[matched] {
            matched = prefix_function[matched - 1];
        }
        if byte == needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            found.push(index + 1 - needle.len()..index + 1);
            // Non-overlapping: the next match starts after this one.
            matched = 0;
        }
    }
    found
}

/// Mask every occurrence of a variable value (of at least three bytes, as for
/// configuration errors) and every credential-shaped token in `text`. For
/// messages built outside the executor from interpolated Compose input,
/// such as a referenced path that contains a substituted value.
pub fn redact_variable_text(text: &str, values: &[String]) -> String {
    let values: Vec<String> = values
        .iter()
        .filter(|value| value.len() >= 3)
        .cloned()
        .collect();
    redact_compose_text(text, &values)
}

/// Every variable value substituted into a resolved Compose model: the
/// project environment, the repository `.env` and include env files. Text
/// derived from the model (paths, log lines) may embed any of them. `Debug`
/// prints only the count, so the values never reach a log through it.
#[derive(Clone, Default)]
pub struct SubstitutedValues(Vec<String>);

impl SubstitutedValues {
    pub(super) fn extend<'a>(&mut self, values: impl IntoIterator<Item = &'a String>) {
        self.0
            .extend(values.into_iter().filter(|value| value.len() >= 3).cloned());
    }

    /// `text` with every substituted value and credential-shaped token
    /// masked.
    pub fn redact(&self, text: &str) -> String {
        redact_compose_text(text, &self.0)
    }
}

impl std::fmt::Debug for SubstitutedValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SubstitutedValues({} values)", self.0.len())
    }
}

/// Mask every known sensitive value and credential-shaped token in `text`,
/// without bounding its length.
pub(super) fn redact_compose_text(text: &str, redact_values: &[String]) -> String {
    redact_byte_ranges(text.as_bytes(), redact_values, None)
}

/// Mask the union of every complete occurrence of a sensitive value in `raw`
/// and the `extra` byte range, decode the result, then mask credential-shaped
/// tokens.
///
/// Occurrences are all found on the original bytes and masked together.
/// Replacing one value at a time would let a shorter value nested in a longer
/// one (a password inside a connection string) mask its part first, leaving
/// the rest of the longer value unrecognised. Decoding happens last, so a
/// value is matched even next to bytes that are not valid UTF-8.
pub(super) fn redact_byte_ranges(
    raw: &[u8],
    redact_values: &[String],
    extra: Option<std::ops::Range<usize>>,
) -> String {
    let mut ranges: Vec<std::ops::Range<usize>> = redact_values
        .iter()
        .flat_map(|value| find_occurrences(raw, value.as_bytes()))
        .collect();
    if let Some(extra) = extra.filter(|range| !range.is_empty()) {
        ranges.push(extra);
    }
    ranges.sort_by_key(|range| range.start);

    let mut masked = Vec::with_capacity(raw.len());
    let mut position = 0;
    let mut previous_was_mask = false;
    for range in ranges {
        if range.end <= position {
            continue;
        }
        let start = range.start.max(position);
        if start > position {
            masked.extend_from_slice(&raw[position..start]);
            previous_was_mask = false;
        }
        if !previous_was_mask {
            masked.extend_from_slice(b"<redacted>");
            previous_was_mask = true;
        }
        position = range.end;
    }
    masked.extend_from_slice(&raw[position..]);

    let mut masked = String::from_utf8_lossy(&masked).into_owned();
    for regex in CREDENTIAL_PATTERNS.iter() {
        masked = regex.replace_all(&masked, "${1}<redacted>").into_owned();
    }
    masked
}

/// Redacted head of an output line cut at [`MAX_STREAMED_LINE_BYTES`].
///
/// The cut may fall inside a sensitive value whose remainder never arrives,
/// leaving its prefix at the end of the head; [`mask_cut_text`] masks it with
/// the rest of the line and then drops the token the cut went through.
pub(super) fn truncated_line_head(head: &[u8], redact_values: &[String]) -> String {
    let masked = mask_cut_text(head, redact_values, CutEdge::End);
    format!(
        "{} … [line truncated after {} bytes]",
        masked.trim_end(),
        MAX_STREAMED_LINE_BYTES
    )
}

/// Docker and application errors commonly echo credentials as assignments,
/// bearer headers, or URI userinfo. Cover those forms even for literal values
/// originating in a repository Compose document. Compiled once: the
/// sanitizer runs on every streamed line of build output.
static CREDENTIAL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#"(?i)(bearer\s+)[A-Za-z0-9._~+/-]+={0,2}"#,
        r#"(://[^\s:/@]+:)[^\s/@]+@"#,
        r#"(?i)((?:password|passwd|token|api[_-]?key|client[_-]?secret|private[_-]?key|authorization)\s*[:=]\s*)[^\s,;]+"#,
    ]
    .into_iter()
    .filter_map(|pattern| Regex::new(pattern).ok())
    .collect()
});

/// Compose can echo values from repository and included env files in stderr.
/// Classify its diagnostic instead of copying untrusted text into a deployment
/// error, where readers may not have permission to view those values.
#[cfg(test)]
fn safe_compose_config_failure(stderr: &[u8]) -> String {
    classify_compose_config_failure(stderr).1
}

static UNKNOWN_FLAG_PATTERN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)\bunknown (?:shorthand )?flag:? '?(--?[a-z0-9][a-z0-9-]{0,63})").ok()
});

/// Classify a `docker compose config` failure and describe it without
/// copying values out of stderr.
fn classify_compose_config_failure(stderr: &[u8]) -> (ComposeConfigFailureClass, String) {
    let diagnostic = String::from_utf8_lossy(stderr);
    let lower = diagnostic.to_ascii_lowercase();
    let (class, cause) = if lower.contains("unknown flag")
        || lower.contains("unknown shorthand flag")
    {
        // Flag names come from Temps' own command line, never from the
        // repository, so the matched name is safe to repeat.
        let flag = UNKNOWN_FLAG_PATTERN
            .as_ref()
            .and_then(|pattern| pattern.captures(&diagnostic))
            .and_then(|captures| captures.get(1).map(|flag| flag.as_str().to_string()))
            .filter(|flag| COMPOSE_CONFIG_FLAGS.contains(&flag.as_str()) || flag.starts_with("--"));
        (
            ComposeConfigFailureClass::UnsupportedComposeVersion,
            format!(
                "The Docker Compose CLI on this host does not support {} that Temps passes to `docker compose config`. Upgrade the Docker Compose plugin to v{MIN_COMPOSE_VERSION} or newer (check with `docker compose version`), then redeploy.",
                flag.map(|flag| format!("the {flag} option"))
                    .unwrap_or_else(|| "an option".to_string())
            ),
        )
    } else if lower.contains("is not a docker command") {
        (
            ComposeConfigFailureClass::UnsupportedComposeVersion,
            format!(
                "The Docker Compose v2 CLI plugin is not installed on this host. Install Docker Compose v{MIN_COMPOSE_VERSION} or newer and verify it with `docker compose version`."
            ),
        )
    } else if (lower.contains("additional property") || lower.contains("additional properties"))
        && lower.contains("'required'")
    {
        (
            ComposeConfigFailureClass::UnsupportedComposeVersion,
            format!(
                "The Docker Compose CLI on this host does not understand env_file 'required'. Upgrade the Docker Compose plugin to v{MIN_COMPOSE_VERSION} or newer."
            ),
        )
    } else if lower.contains("required variable") && lower.contains("missing a value") {
        let variable =
            Regex::new(r"(?i)\brequired variable ([a-z_][a-z0-9_]{0,63}) is missing a value")
                .ok()
                .and_then(|pattern| pattern.captures(&diagnostic))
                .and_then(|captures| captures.get(1).map(|name| name.as_str().to_string()));
        (
            ComposeConfigFailureClass::RequiredVariable,
            match variable {
                Some(variable) => format!("Required Compose variable {variable} has no value. Set it in the project environment or .env file."),
                None => "A required Compose variable has no value. Set it in the project environment or .env file.".to_string(),
            },
        )
    } else if lower.contains("cannot extend service") && lower.contains("not found") {
        (ComposeConfigFailureClass::ExtendsTarget, "An extends reference names a service missing from its referenced Compose file. Check the extends.file and extends.service settings.".to_string())
    } else if lower.contains("depends on undefined service") {
        (ComposeConfigFailureClass::UndefinedDependency, "A service depends on another service that is not defined in the combined Compose project. Check inherited depends_on entries and excluded services.".to_string())
    } else if (lower.contains("env_file") || lower.contains("env file"))
        && (lower.contains("not found") || lower.contains("no such file"))
    {
        (
            ComposeConfigFailureClass::MissingEnvFile,
            "A required env_file is missing. Check its path relative to the Compose project directory, or mark it `required: false`."
                .to_string(),
        )
    } else if lower.contains("no such file") || lower.contains("file not found") {
        (
            ComposeConfigFailureClass::MissingFile,
            "A referenced file is missing. Check Compose file, include, and extends paths."
                .to_string(),
        )
    } else if lower.contains("validating ")
        || lower.contains("additional properties are not allowed")
        || lower.contains("must be a ")
    {
        let field = Regex::new(r"(?i)\bservices\.([a-z0-9_-]{1,64})\.([a-z_]+) must be a[n]? (array|object|boolean|string|integer|number)\b")
            .ok()
            .and_then(|pattern| pattern.captures(&diagnostic))
            .and_then(|captures| {
                let service = captures.get(1)?.as_str();
                let field = captures.get(2)?.as_str();
                let expected = captures.get(3)?.as_str();
                matches!(field, "ports" | "environment" | "env_file" | "volumes" | "networks" | "depends_on" | "healthcheck" | "build" | "image" | "secrets" | "configs" | "labels" | "command" | "entrypoint")
                    .then(|| format!("services.{service}.{field} expects a value of type {expected}."))
            });
        (
            ComposeConfigFailureClass::InvalidModel,
            field.unwrap_or_else(|| "The Compose model has an invalid field or value type. Check the source file and project override.".to_string()),
        )
    } else if lower.contains("yaml:")
        || lower.contains("did not find expected key")
        || lower.contains("mapping values are not allowed")
    {
        (
            ComposeConfigFailureClass::InvalidYaml,
            "A Compose file contains invalid YAML. Check its syntax and indentation.".to_string(),
        )
    } else {
        (
            ComposeConfigFailureClass::Unclassified,
            "Docker Compose rejected the configuration with an error Temps does not recognise. The exact command, its exit code and the sanitized Compose output are in the deployment log; \"Run compose check\" on the deployment page repeats this step and shows the same output."
                .to_string(),
        )
    };

    // Only numeric locations are copied from stderr. File names, field values,
    // and quoted YAML can contain secrets from sources outside Temps' redaction set.
    let line = Regex::new(r"(?i)\bline\s+([1-9][0-9]{0,5})\b")
        .ok()
        .and_then(|pattern| pattern.captures(&diagnostic))
        .and_then(|captures| captures.get(1).map(|line| line.as_str().to_string()));
    let cause = match line {
        Some(line) => format!("{cause} Compose reported line {line}."),
        None => cause,
    };
    (class, cause)
}

/// Exit statuses Docker Compose uses for its own failure classes
/// (`cmd/compose`): 14 compose file not found, 15 parse failure, 16
/// command-line syntax failure (an unknown option).
const COMPOSE_EXIT_FILE_NOT_FOUND: i32 = 14;
const COMPOSE_EXIT_PARSE_FAILURE: i32 = 15;
const COMPOSE_EXIT_COMMAND_SYNTAX: i32 = 16;

/// [`classify_compose_config_failure`], falling back to Compose's exit status
/// when stderr does not identify the failure. Status 16 is what an older
/// Compose returns for an option it does not know.
pub(super) fn classify_compose_config_failure_with_status(
    stderr: &[u8],
    exit_code: Option<i32>,
) -> (ComposeConfigFailureClass, String) {
    let (class, cause) = classify_compose_config_failure(stderr);
    if class != ComposeConfigFailureClass::Unclassified {
        return (class, cause);
    }
    match exit_code {
        Some(COMPOSE_EXIT_COMMAND_SYNTAX) => (
            ComposeConfigFailureClass::UnsupportedComposeVersion,
            format!(
                "Docker Compose rejected the `docker compose config` command line (exit status 16, command syntax failure). This happens when the Compose CLI is older than v{MIN_COMPOSE_VERSION} and does not know an option Temps passes ({}). Upgrade the Docker Compose plugin and check with `docker compose version`.",
                COMPOSE_CONFIG_FLAGS.join(", ")
            ),
        ),
        Some(COMPOSE_EXIT_PARSE_FAILURE) => (
            ComposeConfigFailureClass::InvalidModel,
            "Docker Compose could not parse the combined Compose model (exit status 15). Check the source file, included and extended files, and the project override.".to_string(),
        ),
        Some(COMPOSE_EXIT_FILE_NOT_FOUND) => (
            ComposeConfigFailureClass::MissingFile,
            "Docker Compose could not find a referenced Compose file (exit status 14). Check Compose file, include, and extends paths.".to_string(),
        ),
        _ => (class, cause),
    }
}

/// Most stderr lines and bytes copied into the deployment log.
const MAX_SANITIZED_STDERR_LINES: usize = 40;
const MAX_SANITIZED_STDERR_LINE_BYTES: usize = 300;

/// Patterns that can carry a value in Compose's error text, replaced before
/// stderr reaches a log. What remains is Compose's wording, field paths,
/// option names, error classes and line/column numbers.
static STDERR_VALUE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r#""(?:[^"\\]|\\.)*""#, "\"<value>\""),
        (r"'[^'\n]*'", "'<value>'"),
        (r"`[^`\n]*`", "`<value>`"),
        (r"\$\{[^}\n]*\}?", "<template>"),
        (r"(?i)\b[a-z][a-z0-9+.-]*://\S+", "<url>"),
        (r"\b([A-Za-z_][A-Za-z0-9_]*)=\S+", "${1}=<value>"),
        (r"(^|[\s(\[:=,])/[^\s:,;)\]]+", "${1}<path>"),
    ]
    .into_iter()
    .filter_map(|(pattern, replacement)| Regex::new(pattern).ok().map(|regex| (regex, replacement)))
    .collect()
});

/// Reduce `docker compose` stderr to something safe for any reader of the
/// deployment log: known values redacted, then quoted text, templates,
/// assignments, URLs and absolute paths replaced. Numeric locations and the
/// error's wording survive, so the class of failure stays visible.
pub(super) fn sanitize_config_stderr(stderr: &[u8], redact_values: &[String]) -> String {
    let text = redact_compose_text(&String::from_utf8_lossy(stderr), redact_values);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let skip = lines.len().saturating_sub(MAX_SANITIZED_STDERR_LINES);
    let mut sanitized = Vec::new();
    if skip > 0 {
        sanitized.push(format!("… {skip} earlier line(s) omitted"));
    }
    for line in &lines[skip..] {
        let mut line = (*line).to_string();
        for (pattern, replacement) in STDERR_VALUE_PATTERNS.iter() {
            line = pattern.replace_all(&line, *replacement).into_owned();
        }
        if line.len() > MAX_SANITIZED_STDERR_LINE_BYTES {
            let mut boundary = MAX_SANITIZED_STDERR_LINE_BYTES;
            while !line.is_char_boundary(boundary) {
                boundary -= 1;
            }
            line.truncate(boundary);
            line.push_str(" …");
        }
        sanitized.push(line);
    }
    sanitized.join("\n")
}

/// The argv of `command`, with known values redacted.
pub(super) fn command_argv(
    command: &tokio::process::Command,
    redact_values: &[String],
) -> Vec<String> {
    let command = command.as_std();
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| redact_compose_text(&arg.to_string_lossy(), redact_values))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::support::MIN_COMPOSE_VERSION;
    use crate::compose::test_support::{one_secret, secrets_test_request};
    use crate::compose::{
        ComposeCommandDiagnostics, ComposeConfigFailureClass, ComposeDeployRequest, ComposeError,
        MAX_COMPOSE_DIAGNOSTIC_BYTES,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use temps_presets::compose_check::ComposeCheckErrorClass;

    /// The log tail an official postgres image leaves when the Temps sandbox
    /// denies its entrypoint the capabilities it needs. Shape preserved from a
    /// real failure; no identifying detail.
    const POSTGRES_CAPABILITY_DENIAL_LOG: &str =
        "chmod: /var/lib/postgresql/data: Operation not permitted\n\
         chmod: /var/run/postgresql: Operation not permitted\n\
         error: failed switching to 'postgres': operation not permitted\n";

    #[test]
    fn test_looks_like_capability_denial_detects_entrypoint_privilege_drop() {
        assert!(looks_like_capability_denial(POSTGRES_CAPABILITY_DENIAL_LOG));
        assert!(looks_like_capability_denial(
            "su-exec: setgroups: Operation not permitted"
        ));
        assert!(looks_like_capability_denial(
            "chown: changing ownership of '/data': Operation not permitted"
        ));
    }

    /// The hint must not fire on ordinary application errors — sending someone
    /// to a capability toggle that cannot fix their problem is worse than
    /// leaving the raw logs to speak for themselves.
    #[test]
    fn test_looks_like_capability_denial_ignores_unrelated_failures() {
        // "operation not permitted" with no privileged operation alongside it.
        assert!(!looks_like_capability_denial(
            "FATAL: password authentication failed for user 'app'"
        ));
        assert!(!looks_like_capability_denial(
            "Error: connect ECONNREFUSED 127.0.0.1:5432"
        ));
        assert!(!looks_like_capability_denial(
            "socket bind: Operation not permitted"
        ));
        // A privileged verb with no denial is just normal startup chatter.
        assert!(!looks_like_capability_denial(
            "chown: adjusting ownership of /var/lib/postgresql/data"
        ));
    }

    #[test]
    fn test_redactable_values_keep_secret_and_env_var_sharing_a_key() {
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "secret-value"),
        );
        request
            .environment_vars
            .insert("TOKEN".to_string(), "env-value".to_string());

        let values = collect_redactable_values(&request);

        assert!(values.contains(&"secret-value".to_string()));
        assert!(values.contains(&"env-value".to_string()));
        let diagnostic = sanitize_compose_diagnostic("saw env-value and secret-value", &values);
        assert!(!diagnostic.contains("secret-value"));
        assert!(!diagnostic.contains("env-value"));
    }

    #[test]
    fn compose_diagnostics_redact_known_and_structured_credentials() {
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("MOUNTED", "known-mounted-secret"),
        );
        request.environment_vars.insert(
            "ARBITRARY_NAME".to_string(),
            "known-environment-secret".to_string(),
        );
        request
            .build_args
            .insert("BUILD_VALUE".to_string(), "known-build-secret".to_string());
        request.env_content = Some("PRIVATE_IMAGE_TAG='known-dotenv-secret'\n".to_string());
        let diagnostic = "known-environment-secret known-build-secret known-mounted-secret \
            known-dotenv-secret \
            password=literal-password Authorization: Bearer abc.def.ghi \
            https://user:literal-uri-password@example.test/path";

        let sanitized =
            sanitize_compose_diagnostic(diagnostic, &collect_redactable_values(&request));

        for secret in [
            "known-environment-secret",
            "known-build-secret",
            "known-mounted-secret",
            "known-dotenv-secret",
            "literal-password",
            "abc.def.ghi",
            "literal-uri-password",
        ] {
            assert!(!sanitized.contains(secret), "diagnostic leaked {secret}");
        }
        assert!(sanitized.contains("<redacted>"));
    }

    #[test]
    fn compose_diagnostics_are_bounded_on_utf8_boundaries() {
        let diagnostic = "🔒".repeat(MAX_COMPOSE_DIAGNOSTIC_BYTES);

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &[]);

        assert!(sanitized.len() < diagnostic.len());
        assert!(sanitized.contains("earlier output truncated"));
    }

    #[test]
    fn compose_diagnostics_keep_the_tail_where_the_error_is() {
        // A long BuildKit log: every successful step first, the failure last.
        let mut diagnostic = "#5 [build 2/9] RUN npm ci\n".repeat(4096);
        diagnostic.push_str("#9 ERROR: process \"/bin/sh -c npm run build\" did not complete successfully: exit code: 1\n");
        diagnostic.push_str(
            "failed to solve: process \"/bin/sh -c npm run build\" did not complete successfully",
        );

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &[]);

        assert!(sanitized.len() <= MAX_COMPOSE_DIAGNOSTIC_BYTES + 128);
        assert!(sanitized.starts_with("… earlier output truncated"));
        assert!(sanitized.contains("#9 ERROR: process"), "{sanitized}");
        assert!(sanitized.ends_with("did not complete successfully"));
    }

    #[test]
    fn multi_line_sensitive_values_are_redacted_line_by_line() {
        let key = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n}\n-----END PRIVATE KEY-----";
        let mut request = test_deploy_request_for_redaction();
        request
            .environment_vars
            .insert("SIGNING_KEY".to_string(), key.to_string());

        let values = collect_redactable_values(&request);

        assert!(values.contains(&key.to_string()));
        assert!(values.contains(&"MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".to_string()));
        // Too short to redact on its own without blanking unrelated output.
        assert!(!values.contains(&"}".to_string()));
        assert_eq!(
            sanitize_compose_diagnostic("#7 0.31 MIIEvQIBADANBgkqhkiG9w0BAQEFAASC", &values),
            "#7 0.31 <redacted>"
        );
    }

    fn test_deploy_request_for_redaction() -> ComposeDeployRequest {
        ComposeDeployRequest {
            project_name: "temps-1-1".to_string(),
            compose_content: "services: {}\n".to_string(),
            env_content: None,
            work_dir: PathBuf::from("/tmp"),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
            ready_timeout: None,
            routed_services: Vec::new(),
            output_sink: None,
        }
    }

    #[test]
    fn truncated_line_head_drops_a_split_credential_token() {
        let head = b"pulling from https://deploy:hunter2-long-pass";
        let line = truncated_line_head(head, &[]);
        assert!(!line.contains("hunter2"), "{line}");
        assert!(line.starts_with("pulling from"), "{line}");
    }

    #[test]
    fn values_containing_spaces_are_never_split_at_either_cut_edge() {
        let redact = vec!["swordfish secret".to_string()];

        // A complete value right before a streamed line's cut.
        let line = truncated_line_head(b"build says swordfish secret", &redact);
        assert_eq!(line, "build says … [line truncated after 16384 bytes]");

        // A complete value right after a trimmed capture's cut.
        let mut captured = b"swordfish secret and then the error".to_vec();
        drop_leading_fragment(&mut captured, &redact);
        assert_eq!(captured, "… and then the error".as_bytes());

        // A split value with a space on either side of the cut.
        let line = truncated_line_head(b"build says swordfish sec", &redact);
        assert_eq!(line, "build says … [line truncated after 16384 bytes]");
        let mut captured = b"fish secret and then the error".to_vec();
        drop_leading_fragment(&mut captured, &redact);
        assert_eq!(captured, "… and then the error".as_bytes());
    }

    #[test]
    fn a_cut_inside_a_multibyte_character_still_masks_the_split_value() {
        let value = "swordfish sécret";
        let redact = vec![value.to_string()];
        let e_acute = value.find('é').unwrap();
        // Cut between the two bytes of `é`.
        let split = e_acute + 1;

        let mut head = b"build says ".to_vec();
        head.extend_from_slice(&value.as_bytes()[..split]);
        let line = truncated_line_head(&head, &redact);
        assert_eq!(line, "build says … [line truncated after 16384 bytes]");

        let mut captured = value.as_bytes()[split..].to_vec();
        captured.extend_from_slice(b" and then the error");
        drop_leading_fragment(&mut captured, &redact);
        assert_eq!(captured, "… and then the error".as_bytes());
    }

    #[test]
    fn find_occurrences_returns_non_overlapping_byte_ranges() {
        assert_eq!(find_occurrences(b"abcabcab", b"abc"), vec![0..3, 3..6]);
        assert_eq!(find_occurrences(b"aaaa", b"aa"), vec![0..2, 2..4]);
        assert!(find_occurrences(b"abc", b"").is_empty());
        assert!(find_occurrences(b"ab", b"abc").is_empty());
    }

    #[test]
    fn nested_sensitive_values_are_masked_whole() {
        // Replacing the shorter value first would split the longer one into
        // unrecognised pieces.
        let redact = vec!["inner".to_string(), "outer-inner-tail".to_string()];
        assert_eq!(
            redact_compose_text("x outer-inner-tail y inner z", &redact),
            "x <redacted> y <redacted> z"
        );
    }

    #[test]
    fn truncated_line_head_keeps_text_when_masking_shrinks_it() {
        // Complete 4 KiB values shrink to `<redacted>` when masked, then the
        // cut lands inside a further copy. Only that copy's prefix may go:
        // the readable text around the complete copies must stay.
        let secret = "k".repeat(4096);
        let mut head = format!("step one {secret} step two {secret} step three ").into_bytes();
        head.extend_from_slice(&secret.as_bytes()[..100]);

        let line = truncated_line_head(&head, &[secret]);

        assert_eq!(
            line,
            "step one <redacted> step two <redacted> step three … [line truncated after 16384 bytes]"
        );
    }

    #[test]
    fn value_suffix_prefix_overlap_finds_the_split_value_suffix() {
        assert_eq!(value_suffix_prefix_overlap(b"abcdef", b"def rest"), 3);
        // Only a proper suffix counts: a whole value is masked as usual.
        assert_eq!(value_suffix_prefix_overlap(b"abc", b"abc rest"), 0);
        assert_eq!(value_suffix_prefix_overlap(b"abcabc", b"cabc tail"), 4);
        assert_eq!(value_suffix_prefix_overlap(b"secret", b"unrelated"), 0);
        assert_eq!(value_suffix_prefix_overlap(b"", b"anything"), 0);
        assert_eq!(value_suffix_prefix_overlap(b"abcdef", b""), 0);
    }

    #[test]
    fn compose_config_failures_keep_actionable_cause_without_echoing_secrets() {
        let cases = [
            (
                "validating /checkout/secret.env: services.web.ports must be a array; token=repo-only-secret",
                "services.web.ports expects a value of type array",
            ),
            (
                "cannot extend service \"web\": service \"repo-only-secret\" not found in private.yaml",
                "extends reference names a service missing",
            ),
            (
                "service \"web\" depends on undefined service \"repo-only-secret\": invalid compose project",
                "depends on another service that is not defined",
            ),
            (
                "error while interpolating services.web.image: required variable TOKEN is missing a value: repo-only-secret",
                "Required Compose variable TOKEN has no value",
            ),
            (
                "yaml: line 12: mapping values are not allowed here: repo-only-secret",
                "invalid YAML",
            ),
            (
                "failed to load env_file /checkout/repo-only-secret.env: no such file or directory",
                "required env_file is missing",
            ),
            (
                "unexpected Compose error: repo-only-secret",
                "error Temps does not recognise",
            ),
            (
                "unknown flag: --no-env-resolution",
                "does not support the --no-env-resolution option",
            ),
            (
                "unknown flag: --no-env-resolution\nSee 'docker compose config --help'.",
                "v2.35.0 or newer",
            ),
        ];
        for (diagnostic, expected) in cases {
            let safe = safe_compose_config_failure(diagnostic.as_bytes());
            assert!(safe.contains(expected), "{safe}");
            assert!(!safe.contains("repo-only-secret"), "{safe}");
            assert!(!safe.contains("/checkout/"), "{safe}");
        }
        assert!(safe_compose_config_failure(b"yaml: line 12: bad secret")
            .contains("Compose reported line 12"));
    }

    #[test]
    fn image_inspection_errors_redact_secrets_interpolated_into_image_tags() {
        let secret = "registry-token-that-must-not-leak";
        let diagnostic =
            format!("Docker responded with 404 for registry.example/app:{secret}: image not found");

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &[secret.to_string()]);

        assert!(!sanitized.contains(secret));
        assert!(sanitized.contains("registry.example/app:<redacted>"));
    }

    #[test]
    fn unsupported_compose_flags_map_to_a_specific_error() {
        for (stderr, status) in [
            (&b"unknown flag: --no-env-resolution\n"[..], Some(1)),
            (&b"unknown flag: --no-env-resolution\n"[..], Some(16)),
            (&b""[..], Some(COMPOSE_EXIT_COMMAND_SYNTAX)),
        ] {
            let (class, cause) = classify_compose_config_failure_with_status(stderr, status);
            assert_eq!(class, ComposeConfigFailureClass::UnsupportedComposeVersion);
            assert!(cause.contains(MIN_COMPOSE_VERSION), "{cause}");
            assert!(cause.contains("docker compose version"), "{cause}");
            assert!(!cause.contains("does not recognise"), "{cause}");
            let error = ComposeError::ConfigResolutionFailed {
                project: "temps-1-1".to_string(),
                class,
                exit_status: "16".to_string(),
                cause,
                diagnostics: Box::new(ComposeCommandDiagnostics {
                    argv: vec![],
                    exit_code: status,
                    stderr: String::new(),
                }),
            };
            assert_eq!(
                error.class(),
                ComposeCheckErrorClass::UnsupportedComposeVersion
            );
        }
        let (class, _) = classify_compose_config_failure_with_status(
            b"compose: is not a docker command",
            Some(1),
        );
        assert_eq!(class, ComposeConfigFailureClass::UnsupportedComposeVersion);
    }

    #[test]
    fn config_stderr_is_sanitized_but_keeps_the_error_shape() {
        let stderr = b"validating /checkout/app/compose.yml: services.app.environment.TOKEN must be a string\nline 7: value \"hunter2-secret\" from TOKEN=hunter2-secret at https://user:pw@example.invalid/x\n${SECRET_VAR:-fallback}";
        let sanitized = sanitize_config_stderr(stderr, &["project-variable-value".to_string()]);
        assert!(
            sanitized.contains("services.app.environment.TOKEN must be a string"),
            "{sanitized}"
        );
        assert!(sanitized.contains("line 7"), "{sanitized}");
        for secret in ["hunter2-secret", "/checkout", "user:pw", "fallback"] {
            assert!(!sanitized.contains(secret), "{secret} leaked: {sanitized}");
        }
        let long: Vec<u8> = (0..100)
            .flat_map(|line| format!("line {line}\n").into_bytes())
            .collect();
        let sanitized = sanitize_config_stderr(&long, &[]);
        assert_eq!(sanitized.lines().count(), MAX_SANITIZED_STDERR_LINES + 1);
        assert!(sanitized.ends_with("line 99"));
    }
}
