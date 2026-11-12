// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a stored credential and a service's fixed headers become the headers the
//! worker injects. The SDK's own injection methods render and validate them, so
//! the formats here are exactly the ones the catalog templates describe.
use crate::{
    error::AccessError,
    types::{CredentialDelivery, CredentialFormat},
};
use std::collections::BTreeMap;
use tokenless_core::{
    injection::{
        validate_header_name, InjectedHeader, InjectedHeaders, InjectionSpec, SecretFields,
        SecretMaterial, ALWAYS_STRIPPED,
    },
    variables::{validate_custom_headers, CustomHeader},
};

const FIELD: &str = "secret";
const USERNAME: &str = "username";
const PASSWORD: &str = "password";

/// Name parts that mark a header as carrying a credential: `dd-application-key`,
/// `x-app-secret`, `x-auth-token`, `x-session-id`. A fixed header is stored in
/// the policy in plaintext, shown to everyone who can read the service
/// (owners of linked sandboxes included), and not redacted from responses, so
/// it must never hold a secret. A part matches when it is, starts with or ends
/// with one of these (`apikey`, `authorization`, `accesstoken`). The common
/// credential headers (`x-api-key`, `x-auth-token`) are refused earlier, as
/// [`ALWAYS_STRIPPED`].
const CREDENTIAL_NAME_PARTS: &[&str] = &[
    "key",
    "secret",
    "token",
    "password",
    "passwd",
    "auth",
    "credential",
    "signature",
    "session",
];

/// True when `name` (lower case) reads like a header that carries a credential.
fn names_a_credential(name: &str) -> bool {
    name.split(['-', '_']).any(|part| {
        CREDENTIAL_NAME_PARTS
            .iter()
            .any(|word| part.starts_with(word) || part.ends_with(word))
    })
}

fn invalid(field: &'static str, reason: &'static str) -> AccessError {
    AccessError::Validation { field, reason }
}

fn spec(format: &CredentialFormat) -> InjectionSpec {
    match format {
        CredentialFormat::Bearer => InjectionSpec::Bearer {
            field: FIELD.into(),
        },
        CredentialFormat::Header { name, prefix } => InjectionSpec::Header {
            name: name.clone(),
            field: FIELD.into(),
            prefix: prefix.clone(),
        },
        CredentialFormat::BasicToken { username } => InjectionSpec::BasicToken {
            username: username.clone(),
            password_field: FIELD.into(),
        },
        CredentialFormat::Basic => InjectionSpec::Basic {
            username_field: USERNAME.into(),
            password_field: PASSWORD.into(),
        },
    }
}

/// True when the stored credential is `username:password` rather than one token.
pub fn credential_is_login(delivery: Option<&CredentialDelivery>) -> bool {
    matches!(delivery.map(|d| &d.format), Some(CredentialFormat::Basic))
}

/// True when the credential is base64 encoded into an HTTP Basic header, so it
/// may hold any printable text (a password with spaces or non-ASCII letters)
/// rather than the header-safe bytes a token placed verbatim must be.
pub fn credential_is_encoded(delivery: Option<&CredentialDelivery>) -> bool {
    matches!(
        delivery.map(|d| &d.format),
        Some(CredentialFormat::Basic | CredentialFormat::BasicToken { .. })
    )
}

/// Normalizes header names to lower case and rejects anything the SDK would
/// not inject. A delivery equal to the default (bearer, no headers) is dropped
/// so such policies keep their original stored shape.
pub fn validate_delivery(delivery: &mut Option<CredentialDelivery>) -> Result<(), AccessError> {
    let Some(value) = delivery.as_mut() else {
        return Ok(());
    };
    if let CredentialFormat::Header { name, .. } = &mut value.format {
        *name = validate_header_name(name).map_err(|_| {
            invalid(
                "delivery",
                "the credential header name must be a plain HTTP header name that is not a framing, cookie or proxy header",
            )
        })?;
    }
    spec(&value.format).validate("temps-service").map_err(|_| {
        invalid(
            "delivery",
            "the credential format has an invalid header prefix or Basic username",
        )
    })?;
    let reserved = spec(&value.format).header_names();
    for header in &mut value.headers {
        header.name = validate_header_name(&header.name).map_err(|_| {
            invalid(
                "delivery",
                "request header names must be plain HTTP header names that are not framing, cookie or proxy headers",
            )
        })?;
        if ALWAYS_STRIPPED.contains(&header.name.as_str()) {
            return Err(invalid(
                "delivery",
                "credential headers belong in the credential format, not in fixed request headers",
            ));
        }
        if names_a_credential(&header.name) {
            return Err(invalid(
                "delivery",
                "a request header whose name marks it as a key, secret, token or password cannot be a fixed header: fixed headers are stored and shown as plain text to everyone who can read the service. A service sends one credential, in its credential format",
            ));
        }
    }
    let custom = custom_headers(value);
    let keys = validate_custom_headers(&custom, &reserved).map_err(|_| {
        invalid(
            "delivery",
            "request headers must be unique, at most 16, non-empty printable ASCII, and must not repeat the credential header",
        )
    })?;
    if !keys.is_empty() {
        return Err(invalid(
            "delivery",
            "request header values are fixed text and cannot reference variables",
        ));
    }
    if value.format == CredentialFormat::Bearer && value.headers.is_empty() {
        *delivery = None;
    }
    Ok(())
}

fn custom_headers(delivery: &CredentialDelivery) -> Vec<CustomHeader> {
    delivery
        .headers
        .iter()
        .map(|header| CustomHeader {
            name: header.name.clone(),
            value: header.value.clone(),
        })
        .collect()
}

/// Checks a credential supplied at save time against the format that will send it.
pub fn validate_credential_shape(
    delivery: Option<&CredentialDelivery>,
    secret: &str,
) -> Result<(), AccessError> {
    if delivery.is_some_and(|d| d.format != CredentialFormat::Bearer) && secret.len() > 8192 {
        return Err(invalid(
            "credential",
            "must be at most 8192 bytes for this credential format",
        ));
    }
    if credential_is_login(delivery) {
        match secret.split_once(':') {
            Some((user, pass)) if !user.is_empty() && !pass.is_empty() => {}
            _ => {
                return Err(invalid(
                    "credential",
                    "this service signs in with a username and password: enter them as username:password",
                ))
            }
        }
    }
    Ok(())
}

/// The headers to inject for one grant, and the lower-cased names the worker
/// must strip from the sandbox's own requests before injecting them.
pub fn render(
    delivery: Option<&CredentialDelivery>,
    secret: &str,
) -> Result<(InjectedHeaders, Vec<String>), AccessError> {
    let default = CredentialDelivery {
        format: CredentialFormat::Bearer,
        headers: Vec::new(),
    };
    let delivery = delivery.unwrap_or(&default);
    let spec = spec(&delivery.format);
    if delivery.format == CredentialFormat::Bearer {
        // Bearer tokens keep the 16384-byte limit older services were saved
        // under; the SDK's header renderer stops at 8192.
        let mut headers = InjectedHeaders {
            headers: vec![InjectedHeader::credential(
                "authorization",
                format!("Bearer {secret}"),
            )],
            substitutions: Vec::new(),
            // The worker redacts the token from responses in any form.
            secrets: vec![SecretMaterial::new(secret)],
        };
        return Ok(with_fixed_headers(&mut headers, delivery, &spec));
    }
    let fields = if credential_is_login(Some(delivery)) {
        let (user, pass) = secret.split_once(':').ok_or_else(|| {
            invalid(
                "credential",
                "the stored credential is not in username:password form",
            )
        })?;
        BTreeMap::from([
            (USERNAME.to_string(), user.to_string()),
            (PASSWORD.to_string(), pass.to_string()),
        ])
    } else {
        BTreeMap::from([(FIELD.to_string(), secret.to_string())])
    };
    let mut rendered = spec
        .render("temps-service", &SecretFields::new(fields))
        .map_err(|_| {
            invalid(
                "credential",
                "the stored credential cannot be sent in the selected format",
            )
        })?;
    Ok(with_fixed_headers(&mut rendered, delivery, &spec))
}

/// Appends the fixed headers and returns everything the worker must strip from
/// sandbox requests: the base set, the credential header and every fixed header.
/// Fixed headers are marked as such: they hold no secret (validation refuses
/// credential header names and variables), so the worker does not redact their
/// values from responses, which would break e.g. a reflected `Accept` type.
fn with_fixed_headers(
    rendered: &mut InjectedHeaders,
    delivery: &CredentialDelivery,
    spec: &InjectionSpec,
) -> (InjectedHeaders, Vec<String>) {
    for header in &delivery.headers {
        rendered.headers.push(InjectedHeader::fixed(
            &header.name.to_ascii_lowercase(),
            header.value.clone(),
        ));
    }
    let mut strip = vec![
        "authorization".to_string(),
        "proxy-authorization".to_string(),
        "cookie".to_string(),
    ];
    for name in spec
        .strip_set()
        .into_iter()
        .chain(rendered.headers.iter().map(|h| h.name.clone()))
    {
        if !strip.contains(&name) {
            strip.push(name);
        }
    }
    (
        InjectedHeaders {
            headers: std::mem::take(&mut rendered.headers),
            substitutions: Vec::new(),
            secrets: std::mem::take(&mut rendered.secrets),
        },
        strip,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RequestHeader;

    fn delivery(format: CredentialFormat, headers: &[(&str, &str)]) -> Option<CredentialDelivery> {
        Some(CredentialDelivery {
            format,
            headers: headers
                .iter()
                .map(|(name, value)| RequestHeader {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
        })
    }
    fn header_format(name: &str) -> CredentialFormat {
        CredentialFormat::Header {
            name: name.into(),
            prefix: None,
        }
    }
    fn reason(mut value: Option<CredentialDelivery>) -> String {
        validate_delivery(&mut value).unwrap_err().to_string()
    }

    #[test]
    fn default_delivery_is_dropped_so_old_policies_keep_their_shape() {
        let mut value = delivery(CredentialFormat::Bearer, &[]);
        validate_delivery(&mut value).unwrap();
        assert!(value.is_none());
        let mut value = delivery(
            CredentialFormat::Bearer,
            &[("Notion-Version", "2022-06-28")],
        );
        validate_delivery(&mut value).unwrap();
        assert_eq!(value.unwrap().headers[0].name, "notion-version");
    }

    #[test]
    fn header_names_are_normalized_and_validated() {
        let mut value = delivery(header_format("X-Api-Key"), &[]);
        validate_delivery(&mut value).unwrap();
        assert_eq!(value.unwrap().format, header_format("x-api-key"));
        for bad in [
            "host",
            "cookie",
            "content-length",
            "bad name",
            "proxy-thing",
            "",
        ] {
            assert!(
                reason(delivery(header_format(bad), &[])).contains("delivery"),
                "{bad}"
            );
        }
    }

    /// A second credential typed into a fixed header would be stored and shown
    /// in plain text, so names that mark one are refused; ordinary fixed
    /// headers, including the account identifiers catalog templates send, are
    /// not.
    #[test]
    fn fixed_headers_named_like_credentials_are_refused() {
        for name in [
            "DD-APPLICATION-KEY",
            "x-app-key",
            "x-apikey",
            "x-client-secret",
            "x-refresh-token",
            "x-access-token",
            "x-authority-proof",
            "x-session-id",
            "x-api-signature",
            "x-db-password",
            "x_partner_credential",
        ] {
            let error = reason(delivery(
                header_format("dd-api-key"),
                &[(name, "fixture-value")],
            ));
            assert!(error.contains("plain text"), "{name}: {error}");
        }
        for name in [
            "notion-version",
            "anthropic-version",
            "openai-organization",
            "x-algolia-application-id",
            "plaid-client-id",
            "harvest-account-id",
            "accept",
            "keep-alive-hint",
        ] {
            let mut value = delivery(CredentialFormat::Bearer, &[(name, "fixture-value")]);
            validate_delivery(&mut value).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }

    #[test]
    fn fixed_headers_cannot_carry_credentials_templates_or_duplicates() {
        for headers in [
            vec![("authorization", "Bearer leaked")],
            vec![("x-api-key", "leaked")],
            vec![("private-token", "leaked")],
            vec![("cookie", "a=b")],
            vec![("accept", "{{TOKEN}}")],
            vec![("accept", "")],
            vec![("accept", "text/plain\nx: y")],
            vec![("accept", "a"), ("Accept", "b")],
        ] {
            let mut value = delivery(CredentialFormat::Bearer, &headers);
            assert!(validate_delivery(&mut value).is_err(), "{headers:?}");
        }
        // A fixed header may not repeat the header the credential sets.
        let mut value = delivery(header_format("x-token"), &[("X-Token", "a")]);
        assert!(validate_delivery(&mut value).is_err());
        let many: Vec<(String, String)> =
            (0..17).map(|i| (format!("x-h{i}"), "v".into())).collect();
        let many: Vec<(&str, &str)> = many.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        let mut value = delivery(CredentialFormat::Bearer, &many);
        assert!(validate_delivery(&mut value).is_err());
    }

    #[test]
    fn username_password_credentials_need_both_parts() {
        let login = delivery(CredentialFormat::Basic, &[]);
        validate_credential_shape(login.as_ref(), "user:pass:with:colons").unwrap();
        for bad in ["token-only", ":pass", "user:"] {
            assert!(
                validate_credential_shape(login.as_ref(), bad).is_err(),
                "{bad}"
            );
        }
        validate_credential_shape(None, "token-only").unwrap();
        assert!(render(login.as_ref(), "token-only").is_err());
    }

    /// The worker redacts each format's credential from responses that
    /// reflect it, with the same data plane it runs, and leaves the fixed
    /// headers' values alone (redacting a reflected `Accept` would break
    /// clients that read it back).
    #[test]
    fn every_format_is_redacted_from_responses_and_fixed_headers_are_not() {
        use tokenless_dataplane::scrub::{Scratch, Scrubber};
        let accept = "application/vnd.test+json";
        for (format, stored, raw) in [
            (
                CredentialFormat::Bearer,
                "xoxb-test-0000",
                vec!["xoxb-test-0000"],
            ),
            (
                CredentialFormat::Header {
                    name: "x-api-key".into(),
                    prefix: Some("key=".into()),
                },
                "test+key/0000=",
                vec!["test+key/0000="],
            ),
            (
                CredentialFormat::BasicToken {
                    username: "x-access-token".into(),
                },
                "ghp_test0000aaaa",
                vec!["ghp_test0000aaaa", "x-access-token:ghp_test0000aaaa"],
            ),
            (
                CredentialFormat::Basic,
                "test-user:test-pass/0000",
                vec!["test-pass/0000", "test-user:test-pass/0000"],
            ),
        ] {
            let value = delivery(format.clone(), &[("Accept", accept)]);
            let (headers, _) = render(value.as_ref(), stored).unwrap();
            assert!(!headers.headers[0].fixed, "{format:?}");
            assert!(headers.headers[1].fixed, "{format:?}");
            let listed: Vec<&str> = headers.secrets.iter().map(|s| s.expose()).collect();
            assert_eq!(listed, raw, "{format:?}");
            let scrubber = Scrubber::for_rule(&headers).unwrap();
            let sent = headers.headers[0].value.clone();
            for reflected in [
                raw[0].to_string(),
                raw[0].replace('/', r"\/"),
                raw[0]
                    .replace('/', "%2F")
                    .replace('+', "%2B")
                    .replace('=', "%3D"),
                sent,
            ] {
                let mut body =
                    format!("{{\"echo\":\"{reflected}\",\"accept\":\"{accept}\"}}").into_bytes();
                let count = scrubber.redact_all(&mut body, &mut Scratch::default());
                let body = String::from_utf8(body).unwrap();
                assert!(count >= 1, "{format:?}: {reflected} not found");
                assert!(!body.contains(raw[0]), "{format:?}: {body}");
                assert!(body.contains(accept), "{format:?}: {body}");
            }
        }
    }

    #[test]
    fn basic_token_prefix_and_username_are_checked() {
        let mut value = delivery(
            CredentialFormat::BasicToken {
                username: "has space".into(),
            },
            &[],
        );
        assert!(validate_delivery(&mut value).is_err());
        let mut value = delivery(
            CredentialFormat::Header {
                name: "x-token".into(),
                prefix: Some("bad\nprefix".into()),
            },
            &[],
        );
        assert!(validate_delivery(&mut value).is_err());
    }
}
