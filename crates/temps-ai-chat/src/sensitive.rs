//! Shared handling for AI tool payloads that may contain credentials.

use std::sync::OnceLock;
use temps_core::EncryptionService;

const CIPHERTEXT_FIELD: &str = "__temps_encrypted_v1";
const DISPLAY_FIELD: &str = "__temps_display";

/// Recursively replace values under credential-like keys.
pub(crate) fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    const SENSITIVE: &[&str] = &["value", "secret", "password", "token", "key"];
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let value = if SENSITIVE.iter().any(|needle| lower.contains(needle)) {
                        serde_json::Value::String("***".to_string())
                    } else {
                        redact_value(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(redact_value).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_text(value)),
        _ => value.clone(),
    }
}

/// Encrypt an executable payload while retaining only its redacted review form.
pub(crate) fn encrypt_value(
    encryption: &EncryptionService,
    value: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plaintext = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let ciphertext = encryption
        .encrypt(&plaintext)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        CIPHERTEXT_FIELD: ciphertext,
        DISPLAY_FIELD: redact_value(value),
    }))
}

/// Decrypt a pending-action payload. Legacy plaintext rows remain executable
/// during rolling upgrades; every newly created row uses the encrypted envelope.
pub(crate) fn decrypt_value(
    encryption: &EncryptionService,
    stored: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(ciphertext) = stored
        .get(CIPHERTEXT_FIELD)
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(stored.clone());
    };
    let plaintext = encryption
        .decrypt(ciphertext)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&plaintext).map_err(|error| error.to_string())
}

/// Safe client/storage representation for encrypted or legacy values.
pub(crate) fn display_value(stored: &serde_json::Value) -> serde_json::Value {
    stored
        .get(DISPLAY_FIELD)
        .cloned()
        .unwrap_or_else(|| redact_value(stored))
}

/// Redact a JSON-encoded tool argument/result when possible. Plain text is
/// preserved because it commonly contains useful logs rather than structured
/// credentials; providers must not persist raw structured secret values.
pub(crate) fn redact_json_string(value: &str) -> String {
    serde_json::from_str(value)
        .map(|value| redact_value(&value).to_string())
        .unwrap_or_else(|_| redact_text(value))
}

/// Scrub secrets embedded in CLI command strings or unstructured provider/API
/// errors. `temps_write` intentionally carries a command inside a neutral JSON
/// key, so recursive key redaction alone cannot protect `--value SECRET`.
pub(crate) fn redact_text(value: &str) -> String {
    static ASSIGNMENT_SECRET: OnceLock<regex::Regex> = OnceLock::new();
    static BEARER_SECRET: OnceLock<regex::Regex> = OnceLock::new();
    static PROVIDER_SECRET: OnceLock<regex::Regex> = OnceLock::new();

    let assignments = ASSIGNMENT_SECRET.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\b(password|secret|token|api[_-]?key|access[_-]?key)\b(["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;&}]+)"#,
        )
        .expect("static assignment-secret regex")
    });
    let bearer = BEARER_SECRET.get_or_init(|| {
        regex::Regex::new(r"(?i)\bBearer\s+[^\s,;]+").expect("static bearer-secret regex")
    });
    let provider = PROVIDER_SECRET.get_or_init(|| {
        regex::Regex::new(r"\b(?:sk|key|token)-[A-Za-z0-9._-]{6,}")
            .expect("static provider-secret regex")
    });

    let scrubbed = redact_cli_secret_flags(value);
    let scrubbed = assignments.replace_all(&scrubbed, "$1$2***");
    let scrubbed = bearer.replace_all(&scrubbed, "Bearer ***");
    provider.replace_all(&scrubbed, "[redacted]").into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_payload_round_trips_but_display_is_redacted() {
        let encryption = EncryptionService::new("01234567890123456789012345678901").unwrap();
        let original = serde_json::json!({"name": "db", "password": "very-secret"});
        let stored = encrypt_value(&encryption, &original).unwrap();
        assert_ne!(stored.to_string(), original.to_string());
        assert_eq!(decrypt_value(&encryption, &stored).unwrap(), original);
        assert_eq!(display_value(&stored)["password"], "***");
        assert!(!stored.to_string().contains("very-secret"));
    }

    #[test]
    fn write_command_arguments_are_safe_for_sse_and_history() {
        let arguments = serde_json::json!({
            "command": "projects create_environment_variable --name DATABASE_URL --value 'postgres://user:pass@db/app' --api-key sk-live-abcdef"
        })
        .to_string();
        let display = redact_json_string(&arguments);
        assert!(!display.contains("postgres://user:pass@db/app"));
        assert!(!display.contains("sk-live-abcdef"));
        assert!(display.contains("--value ***"));
        assert!(display.contains("--api-key ***"));
    }

    #[test]
    fn write_command_redaction_consumes_complete_punctuation_values() {
        let secret = "VERY_SECRET";
        for command in [
            format!(
                r#"projects create_environment_variable --name CONFIG --value {{"first":"public","second":"{secret}"}}"#
            ),
            format!("projects create_environment_variable --name CONFIG --value abc;{secret}"),
        ] {
            let arguments = serde_json::json!({"command": command}).to_string();
            let display = redact_json_string(&arguments);
            assert!(!display.contains(secret), "secret leaked from {arguments}");
            assert!(display.contains("--value ***"));
        }
    }

    #[test]
    fn write_command_redaction_matches_mixed_and_unterminated_quote_tokens() {
        let secret = "VERY_SECRET";
        for command in [
            format!("projects create_environment_variable --name X --value 'prefix'{secret}"),
            format!("projects create_environment_variable --name X --value 'prefix {secret}"),
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(!display.contains(secret), "secret leaked from {command}");
            assert!(display.contains("--value ***"));
        }
    }

    #[test]
    fn bare_sensitive_boolean_does_not_hide_the_following_secret_flag() {
        for command in [
            "operation --token --value VERY_SECRET",
            "operation --value --token VERY_SECRET",
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(
                !display.contains("VERY_SECRET"),
                "secret leaked from {command}"
            );
        }
    }

    #[test]
    fn quoted_or_mixed_sensitive_flag_names_are_classified_like_execution() {
        for command in [
            r#"operation "--value" VERY_SECRET"#,
            "operation --'value' VERY_SECRET",
            "operation --'api-key'=VERY_SECRET",
        ] {
            let display = redact_json_string(&serde_json::json!({"command": command}).to_string());
            assert!(
                !display.contains("VERY_SECRET"),
                "secret leaked from {command}"
            );
        }
    }

    #[test]
    fn unstructured_upstream_errors_are_scrubbed() {
        let error = r#"HTTP 400: {"password":"hunter2","detail":"Bearer token-abcdef rejected"}"#;
        let display = redact_text(error);
        assert!(!display.contains("hunter2"));
        assert!(!display.contains("token-abcdef"));
    }
}
