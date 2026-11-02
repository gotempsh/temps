// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recommends marking a regular environment variable as a secret.
//!
//! This is advice, never enforcement: nothing here blocks a write or changes
//! a flag. Marking a variable secret is one-way and makes it write-only, so a
//! wrong suggestion costs the operator the ability to read a harmless value.
//! The rules therefore aim for precision over recall, and lean on the value
//! as well as the name: `DATABASE_URL=postgres://localhost/app` holds no
//! credential, `DATABASE_URL=postgres://app:hunter2@db/app` does.
//!
//! This is deliberately not the import or compose redaction heuristics
//! (`temps_import_types::plan::looks_like_secret_env_key`,
//! `temps_presets`' `is_secret_key_name`): those decide what to hide from a
//! response, where over-matching is the safe failure.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Why a regular variable looks like it holds a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretSuggestionReason {
    /// A URL or connection string with an embedded password.
    ConnectionStringPassword,
    /// A PEM-encoded private key.
    PrivateKey,
    /// A token in a format a well-known provider issues.
    ProviderToken,
    /// A JSON Web Token.
    JsonWebToken,
    /// The name says credential and the value looks like one.
    CredentialName,
}

impl SecretSuggestionReason {
    /// One sentence for the operator, naming the variable.
    pub fn message(self, key: &str) -> String {
        match self {
            Self::ConnectionStringPassword => {
                format!("{key} contains a password in its connection string")
            }
            Self::PrivateKey => format!("{key} contains a private key"),
            Self::ProviderToken => format!("{key} contains an API token"),
            Self::JsonWebToken => format!("{key} contains a signed token (JWT)"),
            Self::CredentialName => format!("{key} is named like a credential"),
        }
    }
}

/// Words that, as a whole segment of the name, mean the value is a
/// credential. Matched per `_`/`-`/`.` segment so `PASSWORD_MIN_LENGTH`
/// still needs a credential-looking value to count.
const CREDENTIAL_WORDS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "APIKEY",
    "CREDENTIAL",
    "CREDENTIALS",
];

/// Two-segment names that are credentials even though neither word is on
/// its own (`KEY` alone covers `CACHE_KEY`, `KEY_PREFIX`, ...).
const CREDENTIAL_PAIRS: &[(&str, &str)] = &[
    ("API", "KEY"),
    ("ACCESS", "KEY"),
    ("SECRET", "KEY"),
    ("PRIVATE", "KEY"),
    ("SIGNING", "KEY"),
    ("ENCRYPTION", "KEY"),
    ("MASTER", "KEY"),
    ("CLIENT", "SECRET"),
];

/// Prefixes frameworks compile into the browser bundle. Such a value is
/// public by construction, so the name alone is not a reason to hide it.
const PUBLIC_PREFIXES: &[&str] = &[
    "PUBLIC_",
    "NEXT_PUBLIC_",
    "VITE_",
    "REACT_APP_",
    "EXPO_PUBLIC_",
];

/// Prefixes of tokens issued by widely used providers.
const TOKEN_PREFIXES: &[&str] = &[
    "sk_live_",
    "rk_live_",
    "sk_test_",
    "whsec_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "npm_",
    "SG.",
    "sk-ant-",
    "sk-proj-",
];

/// Returns why `value` should probably be stored as a secret, or `None`.
pub fn secret_suggestion(key: &str, value: &str) -> Option<SecretSuggestionReason> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.contains("-----BEGIN") && value.contains("PRIVATE KEY-----") {
        return Some(SecretSuggestionReason::PrivateKey);
    }
    if has_connection_string_password(value) {
        return Some(SecretSuggestionReason::ConnectionStringPassword);
    }
    if is_provider_token(value) {
        return Some(SecretSuggestionReason::ProviderToken);
    }
    if is_json_web_token(value) {
        return Some(SecretSuggestionReason::JsonWebToken);
    }
    if is_credential_name(key) && looks_like_credential_value(value) {
        return Some(SecretSuggestionReason::CredentialName);
    }
    None
}

/// `scheme://user:password@host/...` with a non-empty password, or an
/// ADO/JDBC-style `...;Password=...;` string.
fn has_connection_string_password(value: &str) -> bool {
    if let Some((_, rest)) = value.split_once("://") {
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if let Some((userinfo, _host)) = authority.rsplit_once('@') {
            if let Some((_user, password)) = userinfo.split_once(':') {
                return !password.is_empty();
            }
        }
        return false;
    }
    value.split(';').any(|part| {
        part.split_once('=').is_some_and(|(name, password)| {
            let name = name.trim().to_ascii_lowercase();
            (name == "password" || name == "pwd") && !password.trim().is_empty()
        })
    })
}

fn is_provider_token(value: &str) -> bool {
    if value.contains(char::is_whitespace) {
        return false;
    }
    if TOKEN_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix) && value.len() >= prefix.len() + 16)
    {
        return true;
    }
    // AWS access key ID: AKIA followed by 16 upper-case letters or digits.
    value.len() == 20
        && value.starts_with("AKIA")
        && value[4..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn is_json_web_token(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    segments.len() == 3
        && segments[0].starts_with("eyJ")
        && segments.iter().all(|segment| {
            segment.len() >= 4
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=')
        })
}

fn is_credential_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    if PUBLIC_PREFIXES
        .iter()
        .any(|prefix| upper.starts_with(prefix))
    {
        return false;
    }
    let segments: Vec<&str> = upper.split(['_', '-', '.']).collect();
    segments
        .iter()
        .any(|segment| CREDENTIAL_WORDS.contains(segment))
        || segments
            .windows(2)
            .any(|pair| CREDENTIAL_PAIRS.contains(&(pair[0], pair[1])))
}

/// Rules out the settings that share a credential's name: lengths, TTLs,
/// toggles and other short or numeric values (`TOKEN_TTL=3600`,
/// `PASSWORD_RESET_ENABLED=true`), and URLs without a password
/// (`TOKEN_URL=https://auth.example.com/token`).
fn looks_like_credential_value(value: &str) -> bool {
    if value.len() < 8 || value.contains(char::is_whitespace) {
        return false;
    }
    if value.contains("://") {
        return false;
    }
    if value.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "false" | "enabled" | "disabled" | "required" | "optional"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_string_with_password_is_flagged() {
        for value in [
            "postgres://app:hunter2@db.internal:5432/app",
            "postgresql://app:hunter2@db/app?sslmode=require",
            "redis://:s3cret@cache:6379/0",
            "mongodb+srv://app:hunter2@cluster.example.net/db",
            "Server=db;Database=app;User Id=app;Password=hunter2;",
        ] {
            assert_eq!(
                secret_suggestion("DATABASE_URL", value),
                Some(SecretSuggestionReason::ConnectionStringPassword),
                "{value}"
            );
        }
    }

    #[test]
    fn connection_string_without_password_is_not_flagged() {
        for value in [
            "postgres://localhost/app",
            "postgres://app@db/app",
            "redis://cache:6379/0",
            "https://api.example.com/v1?redirect=https://user:pw@elsewhere",
        ] {
            assert_eq!(secret_suggestion("DATABASE_URL", value), None, "{value}");
        }
    }

    #[test]
    fn private_key_is_flagged() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----";
        assert_eq!(
            secret_suggestion("SSH_KEY", pem),
            Some(SecretSuggestionReason::PrivateKey)
        );
        let public = "-----BEGIN PUBLIC KEY-----\nMIIBIj\n-----END PUBLIC KEY-----";
        assert_eq!(secret_suggestion("JWT_PUBLIC_KEY", public), None);
    }

    #[test]
    fn provider_tokens_are_flagged_whatever_the_name() {
        for value in [
            "sk_live_0123456789abcdefABCDEF",
            "ghp_0123456789abcdefghijABCDEFGHIJ012345",
            "glpat-0123456789abcdefghij",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert_eq!(
                secret_suggestion("SOMETHING", value),
                Some(SecretSuggestionReason::ProviderToken),
                "{value}"
            );
        }
        // A prefix alone is not a token.
        assert_eq!(secret_suggestion("MODE", "sk_live_"), None);
    }

    #[test]
    fn a_leaked_token_in_a_public_variable_is_still_flagged() {
        assert_eq!(
            secret_suggestion("NEXT_PUBLIC_STRIPE_KEY", "sk_live_0123456789abcdefABCDEF"),
            Some(SecretSuggestionReason::ProviderToken)
        );
    }

    #[test]
    fn json_web_token_is_flagged() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXZhbHVl";
        assert_eq!(
            secret_suggestion("SERVICE_ROLE", jwt),
            Some(SecretSuggestionReason::JsonWebToken)
        );
    }

    #[test]
    fn credential_names_with_credential_values_are_flagged() {
        for key in [
            "DB_PASSWORD",
            "JWT_SECRET",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "OAUTH_CLIENT_SECRET",
            "MAILER_APIKEY",
        ] {
            assert_eq!(
                secret_suggestion(key, "q8Zt3kLm9PxR"),
                Some(SecretSuggestionReason::CredentialName),
                "{key}"
            );
        }
    }

    #[test]
    fn settings_named_like_credentials_are_not_flagged() {
        for (key, value) in [
            ("TOKEN_TTL", "3600"),
            ("PASSWORD_MIN_LENGTH", "12"),
            ("PASSWORD_RESET_ENABLED", "true"),
            ("TOKEN_URL", "https://auth.example.com/oauth/token"),
            ("SECRET_NAME", "app"),
        ] {
            assert_eq!(secret_suggestion(key, value), None, "{key}={value}");
        }
    }

    #[test]
    fn plain_configuration_is_not_flagged() {
        for (key, value) in [
            ("NODE_ENV", "production"),
            ("PORT", "3000"),
            ("CACHE_KEY", "user-profile-v2"),
            ("KEY_PREFIX", "app:sessions"),
            ("API_URL", "https://api.example.com"),
            ("LOG_LEVEL", "debug"),
            ("EMPTY_PASSWORD", ""),
        ] {
            assert_eq!(secret_suggestion(key, value), None, "{key}={value}");
        }
    }

    #[test]
    fn public_framework_variables_are_not_flagged_by_name() {
        assert_eq!(
            secret_suggestion("NEXT_PUBLIC_ANALYTICS_TOKEN", "pk_0123456789abcdef"),
            None
        );
        assert_eq!(
            secret_suggestion("VITE_MAPS_API_KEY", "AIzaSyD0123456789abcdef"),
            None
        );
    }

    #[test]
    fn message_names_the_variable() {
        assert_eq!(
            SecretSuggestionReason::ConnectionStringPassword.message("DATABASE_URL"),
            "DATABASE_URL contains a password in its connection string"
        );
    }
}
