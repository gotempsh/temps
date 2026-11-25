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
    value: String,
    is_secret: bool,
    generate: Option<GenerateValueRequest>,
) -> Result<(String, bool), EnvVarError> {
    let Some(generate) = generate else {
        return Ok((value, is_secret));
    };
    if !value.is_empty() {
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
    }
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
        let (value, is_secret) = resolve_create_value("PORT", "3000".into(), false, None).unwrap();
        assert_eq!(value, "3000");
        assert!(!is_secret);
    }

    #[test]
    fn a_generated_secret_is_random_hex_and_always_secret() {
        let (a, is_secret) =
            resolve_create_value("SESSION_SECRET", String::new(), false, secret(32)).unwrap();
        let (b, _) =
            resolve_create_value("SESSION_SECRET", String::new(), false, secret(32)).unwrap();
        assert!(is_secret, "a generated value is stored write-only");
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn value_and_generate_together_are_refused() {
        let error = resolve_create_value("API_KEY", "typed".into(), true, secret(32)).unwrap_err();
        assert!(error.to_string().contains("API_KEY"));
        assert!(error.to_string().contains("not both"));
    }

    #[test]
    fn sizes_outside_the_bounds_are_refused() {
        for bytes in [0, 15, 65, 4096] {
            let error = resolve_create_value("K", String::new(), true, secret(bytes)).unwrap_err();
            assert!(error.to_string().contains(&bytes.to_string()), "{error}");
        }
        assert!(resolve_create_value("K", String::new(), true, secret(16)).is_ok());
        assert!(resolve_create_value("K", String::new(), true, secret(64)).is_ok());
    }

    #[test]
    fn bytes_default_to_32() {
        let request: GenerateValueRequest =
            serde_json::from_value(serde_json::json!({"kind": "secret"})).unwrap();
        assert_eq!(request.bytes, DEFAULT_GENERATED_BYTES);
    }
}
