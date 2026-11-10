// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credential-safe presentation shared by chat and external MCP.
use std::sync::OnceLock;
/// Recursively replace values under credential-like keys.
pub fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    const SENSITIVE: &[&str] = &[
        "value",
        "secret",
        "password",
        "token",
        "key",
        "credential",
        "authorization",
        "cookie",
        "headers",
        "webhook",
    ];
    match value {
        serde_json::Value::Object(object) => {
            // Generic webhook provider configs use the deliberately neutral
            // field name `url`. In that schema the sibling method/headers
            // fields identify it as a delivery endpoint whose path commonly
            // embeds a credential; ordinary public URLs remain visible.
            let webhook_config = object.contains_key("url")
                && object.contains_key("method")
                && object.contains_key("headers");
            serde_json::Value::Object(
                object
                    .iter()
                    .map(|(key, value)| {
                        let lower = key.to_ascii_lowercase();
                        let sensitive = SENSITIVE.iter().any(|needle| lower.contains(needle))
                            || (webhook_config && lower == "url");
                        let value = if sensitive {
                            serde_json::Value::String("***".to_string())
                        } else {
                            redact_value(value)
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(redact_value).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_text(value)),
        _ => value.clone(),
    }
}

/// Redact a JSON-encoded tool argument/result when possible. Plain text is
/// preserved because it commonly contains useful logs rather than structured
/// credentials; providers must not persist raw structured secret values.
pub fn redact_json_string(value: &str) -> String {
    serde_json::from_str(value)
        .map(|value| redact_value(&value).to_string())
        .unwrap_or_else(|_| redact_text(value))
}

/// Scrub secrets embedded in CLI command strings or unstructured provider/API
/// errors. `temps_write` intentionally carries a command inside a neutral JSON
/// key, so recursive key redaction alone cannot protect `--value SECRET`.
pub fn redact_text(value: &str) -> String {
    static ASSIGNMENT_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();
    static BEARER_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();
    static PROVIDER_SECRET: OnceLock<Option<regex::Regex>> = OnceLock::new();

    let assignments = ASSIGNMENT_SECRET.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\b(password|secret|token|api[_-]?key|access[_-]?key)\b(["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;&}]+)"#,
        )
        .ok()
    });
    let bearer = BEARER_SECRET.get_or_init(|| regex::Regex::new(r"(?i)\bBearer\s+[^\s,;]+").ok());
    let provider = PROVIDER_SECRET
        .get_or_init(|| regex::Regex::new(r"\b(?:sk|key|token)-[A-Za-z0-9._-]{6,}").ok());

    let scrubbed = redact_cli_secret_flags(value);
    let scrubbed = assignments.as_ref().map_or_else(
        || scrubbed.clone(),
        |pattern| pattern.replace_all(&scrubbed, "$1$2***").into_owned(),
    );
    let scrubbed = bearer.as_ref().map_or_else(
        || scrubbed.clone(),
        |pattern| pattern.replace_all(&scrubbed, "Bearer ***").into_owned(),
    );
    provider.as_ref().map_or(scrubbed.clone(), |pattern| {
        pattern.replace_all(&scrubbed, "[redacted]").into_owned()
    })
}

/// Redact complete CLI value tokens using the same quote-aware token boundary
/// rules as `temps-ai-api-tools::cli::tokenize`. This deliberately handles
/// mixed quoted/unquoted tokens and the parser's legacy acceptance of an
/// unmatched quote; both are impossible to model safely with a "quoted value"
/// regex without leaving a suffix visible.
fn redact_cli_secret_flags(input: &str) -> String {
    let spans = cli_token_spans(input);
    let mut replacements = Vec::new();
    let mut index = 0;
    while index < spans.len() {
        let (start, end) = spans[index];
        let raw_token = &input[start..end];
        let token = normalize_cli_token(raw_token);
        if let Some((flag, _)) = token.split_once('=') {
            if is_sensitive_cli_flag(flag) {
                if let Some(equals) = raw_token.find('=') {
                    replacements.push((start + equals + 1, end));
                }
            }
        } else if is_sensitive_cli_flag(&token) && index + 1 < spans.len() {
            let next = spans[index + 1];
            if !normalize_cli_token(&input[next.0..next.1]).starts_with("--") {
                replacements.push(next);
                index += 1;
            }
        }
        index += 1;
    }

    let mut output = input.to_string();
    for (start, end) in replacements.into_iter().rev() {
        output.replace_range(start..end, "***");
    }
    output
}

fn normalize_cli_token(raw: &str) -> String {
    let mut normalized = String::new();
    let mut quote = None;
    for character in raw.chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => normalized.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None => normalized.push(character),
        }
    }
    normalized
}

fn is_sensitive_cli_flag(token: &str) -> bool {
    let Some(flag) = token.strip_prefix("--") else {
        return false;
    };
    let flag = flag.to_ascii_lowercase();
    flag == "value"
        || flag == "config"
        || ["password", "secret", "token", "key"]
            .iter()
            .any(|needle| flag.contains(needle))
}

fn cli_token_spans(input: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut quote = None;
    for (index, character) in input.char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if character == '\'' || character == '"' => {
                start.get_or_insert(index);
                quote = Some(character);
            }
            None if character.is_whitespace() => {
                if let Some(start) = start.take() {
                    spans.push((start, index));
                }
            }
            None => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(start) = start {
        spans.push((start, input.len()));
    }
    spans
}
