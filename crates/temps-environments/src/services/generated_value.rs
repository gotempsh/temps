// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Values Temps generates for a new environment variable, so a secret such
//! as `SESSION_SECRET` never passes through the browser's form state or a
//! request log: the client asks for one, the server makes it and stores it
//! write-only.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::EnvVarError;

/// Fewest random bytes a generated secret may have.
pub const MIN_GENERATED_BYTES: u32 = 16;
/// Most random bytes a generated secret may have.
pub const MAX_GENERATED_BYTES: u32 = 64;
/// Bytes used when the request does not say.
pub const DEFAULT_GENERATED_BYTES: u32 = 32;

/// What to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedValueKind {
    /// Random bytes, hex encoded (`bytes * 2` characters).
    Secret,
    /// A Laravel application key (`APP_KEY`): 32 random bytes, base64
    /// encoded behind `base64:`, as `php artisan key:generate` writes it.
    /// Its default cipher (AES-256-CBC) needs exactly 32 bytes, so `bytes`
    /// must be 32 or left out.
    LaravelAppKey,
}

/// Ask the server to generate the value instead of sending one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenerateValueRequest {
    pub kind: GeneratedValueKind,
    /// Random bytes, from 16 to 64; 32 when omitted.
    #[serde(default = "default_bytes")]
    #[schema(default = 32, minimum = 16, maximum = 64)]
    pub bytes: u32,
}

fn default_bytes() -> u32 {
    DEFAULT_GENERATED_BYTES
}

/// The value to store and whether it must be secret, from what the request
/// sent: either a value, or a request to generate one (never both). A
/// generated value is always stored write-only.
pub fn resolve_create_value(
    key: &str,
    value: Option<String>,
    is_secret: bool,
    generate: Option<GenerateValueRequest>,
) -> Result<(String, bool), EnvVarError> {
    let Some(generate) = generate else {
        // A value is required unless the server is asked to make one; an
        // empty one is kept only when it was sent.
        return value.map(|value| (value, is_secret)).ok_or_else(|| {
            EnvVarError::InvalidInput(format!(
                "Environment variable '{key}' needs a value, or 'generate' to have the server make one"
            ))
        });
    };
    if value.as_deref().is_some_and(|value| !value.is_empty()) {
        return Err(EnvVarError::InvalidInput(format!(
            "Environment variable '{key}': send either a value or 'generate', not both"
        )));
    }
    if !(MIN_GENERATED_BYTES..=MAX_GENERATED_BYTES).contains(&generate.bytes) {
        return Err(EnvVarError::InvalidInput(format!(
            "Environment variable '{key}': a generated secret has from {MIN_GENERATED_BYTES} to \
             {MAX_GENERATED_BYTES} random bytes, not {}",
            generate.bytes
        )));
    }
    match generate.kind {
        GeneratedValueKind::Secret => Ok((random_hex(generate.bytes as usize), true)),
        GeneratedValueKind::LaravelAppKey if generate.bytes != LARAVEL_KEY_BYTES => {
            Err(EnvVarError::InvalidInput(format!(
                "Environment variable '{key}': a Laravel application key is always \
                 {LARAVEL_KEY_BYTES} random bytes, not {}; leave 'bytes' out",
                generate.bytes
            )))
        }
        GeneratedValueKind::LaravelAppKey => Ok((laravel_app_key(), true)),
    }
}

/// The key length Laravel's default cipher (AES-256-CBC) needs.
const LARAVEL_KEY_BYTES: u32 = 32;

/// A key Laravel's default cipher accepts: 32 random bytes, base64 encoded
/// behind `base64:`, which tells Laravel to decode it.
fn laravel_app_key() -> String {
    use base64::Engine as _;
    let bytes: [u8; LARAVEL_KEY_BYTES as usize] = rand::random();
    format!(
        "base64:{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// `bytes` random bytes from the thread's CSPRNG, hex encoded.
fn random_hex(bytes: usize) -> String {
    use rand::RngExt;
    use std::fmt::Write;
    let mut rng = rand::rng();
    let mut out = String::with_capacity(bytes * 2);
    for _ in 0..bytes {
        let byte: u8 = rng.random();
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(bytes: u32) -> Option<GenerateValueRequest> {
        Some(GenerateValueRequest {
            kind: GeneratedValueKind::Secret,
            bytes,
        })
    }

    #[test]
    fn without_generate_the_value_is_kept() {
        let (value, is_secret) =
            resolve_create_value("PORT", Some("3000".into()), false, None).unwrap();
        assert_eq!(value, "3000");
        assert!(!is_secret);
    }

    /// Leaving out both the value and `generate` is a mistake, refused
    /// before anything is stored; an empty value sent on purpose is kept.
    #[test]
    fn a_value_is_required_unless_generated() {
        let error = resolve_create_value("PORT", None, false, None).unwrap_err();
        assert!(matches!(error, EnvVarError::InvalidInput(_)));
        assert!(error.to_string().contains("PORT"), "{error}");
        assert_eq!(
            resolve_create_value("EMPTY", Some(String::new()), false, None).unwrap(),
            (String::new(), false)
        );
        // An empty value beside `generate` is no value.
        assert!(resolve_create_value("K", Some(String::new()), true, secret(32)).is_ok());
    }

    #[test]
    fn a_generated_secret_is_random_hex_and_always_secret() {
        let (a, is_secret) =
            resolve_create_value("SESSION_SECRET", None, false, secret(32)).unwrap();
        let (b, _) = resolve_create_value("SESSION_SECRET", None, false, secret(32)).unwrap();
        assert!(is_secret, "a generated value is stored write-only");
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn value_and_generate_together_are_refused() {
        let error =
            resolve_create_value("API_KEY", Some("typed".into()), true, secret(32)).unwrap_err();
        assert!(error.to_string().contains("API_KEY"));
        assert!(error.to_string().contains("not both"));
    }

    #[test]
    fn sizes_outside_the_bounds_are_refused() {
        for bytes in [0, 15, 65, 4096] {
            let error = resolve_create_value("K", None, true, secret(bytes)).unwrap_err();
            assert!(error.to_string().contains(&bytes.to_string()), "{error}");
        }
        assert!(resolve_create_value("K", None, true, secret(16)).is_ok());
        assert!(resolve_create_value("K", None, true, secret(64)).is_ok());
    }

    /// Every key, `APP_KEY` included, gets what it asked for: `bytes * 2`
    /// hex characters. A client that needs another format (Laravel's
    /// 32-character key) asks for the length that gives it.
    #[test]
    fn every_key_gets_the_hex_secret_it_asked_for() {
        for (key, bytes) in [("APP_KEY", 64), ("APP_KEY", 16), ("SESSION_SECRET", 32)] {
            let (value, is_secret) = resolve_create_value(key, None, false, secret(bytes)).unwrap();
            assert_eq!(value.len(), bytes as usize * 2, "{key}");
            assert!(
                value.bytes().all(|c| c.is_ascii_hexdigit()),
                "{key}: {value}"
            );
            assert!(is_secret);
        }
    }

    /// Asked for explicitly, a Laravel key is the 32 bytes its cipher
    /// needs, behind `base64:`; another length is refused, not ignored.
    #[test]
    fn a_laravel_app_key_is_the_32_bytes_its_cipher_needs() {
        use base64::Engine as _;
        let laravel = |bytes| {
            resolve_create_value(
                "APP_KEY",
                None,
                false,
                Some(GenerateValueRequest {
                    kind: GeneratedValueKind::LaravelAppKey,
                    bytes,
                }),
            )
        };
        let (value, is_secret) = laravel(DEFAULT_GENERATED_BYTES).unwrap();
        assert!(is_secret);
        let encoded = value
            .strip_prefix("base64:")
            .expect("Laravel's base64: prefix");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(decoded.len(), 32);
        assert_ne!(laravel(32).unwrap().0, value);
        let error = laravel(64).unwrap_err().to_string();
        assert!(error.contains("APP_KEY") && error.contains("64"), "{error}");
        let request: GenerateValueRequest =
            serde_json::from_value(serde_json::json!({"kind": "laravel_app_key"})).unwrap();
        assert_eq!(request.kind, GeneratedValueKind::LaravelAppKey);
    }

    #[test]
    fn bytes_default_to_32() {
        let request: GenerateValueRequest =
            serde_json::from_value(serde_json::json!({"kind": "secret"})).unwrap();
        assert_eq!(request.bytes, DEFAULT_GENERATED_BYTES);
    }
}
