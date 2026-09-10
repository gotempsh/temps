// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared path policy for files published by static deployments.
//!
//! The same policy is applied when artifacts are ingested and immediately before
//! the proxy resolves a request. This prevents an accidentally packaged secret
//! from becoming public even when one of those boundaries is bypassed.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Maximum size of one publicly served immutable static asset.
///
/// Filesystem deployments stream larger ordinary files, but CAS fallback uses
/// this limit at both persistence and serving boundaries.
pub const MAX_PUBLIC_STATIC_ASSET_BYTES: u64 = 32 * 1024 * 1024;

/// Maximum URL path retained or resolved by static-file infrastructure.
pub const MAX_STATIC_ASSET_URL_PATH_BYTES: usize = 4 * 1024;

/// Maximum number of components accepted in static artifact and deployment paths.
///
/// This is shared with archive extraction and recursive-copy boundaries so an
/// image cannot manufacture path trees deep enough to exhaust the process stack.
pub const MAX_STATIC_PATH_COMPONENTS: usize = 64;

/// Why a path cannot be used by the static deployment subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticPathPolicyError {
    Empty,
    Absolute,
    NotClean,
    TooLong,
    TooDeep,
    NonUtf8,
    Sensitive,
}

impl fmt::Display for StaticPathPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::Empty => "path is empty",
            Self::Absolute => "path must be relative",
            Self::NotClean => "path must contain only clean relative components",
            Self::TooLong => "path exceeds the static asset URL length limit",
            Self::TooDeep => "path exceeds the static path component limit",
            Self::NonUtf8 => "path is not valid UTF-8",
            Self::Sensitive => "path is reserved or may contain sensitive deployment data",
        };
        formatter.write_str(reason)
    }
}

impl std::error::Error for StaticPathPolicyError {}

/// Validate a relative file path before it enters a static deployment.
pub fn validate_static_artifact_path(path: &Path) -> Result<(), StaticPathPolicyError> {
    validate_clean_relative_path(path, false)?;
    if is_sensitive_static_path(path) {
        return Err(StaticPathPolicyError::Sensitive);
    }
    Ok(())
}

/// Validate and return a stored static deployment directory.
///
/// Stored locations are always relative to the operator-controlled static root.
/// Normalizing absolute or parent-relative database values would turn corrupted
/// state into an arbitrary filesystem lookup, so non-clean values are rejected.
pub fn validate_static_dir(path: &str) -> Result<PathBuf, StaticPathPolicyError> {
    if path.is_empty() {
        return Err(StaticPathPolicyError::Empty);
    }
    if path.contains('\0') || path.contains('\\') {
        return Err(StaticPathPolicyError::NotClean);
    }
    if path.len() > MAX_STATIC_ASSET_URL_PATH_BYTES {
        return Err(StaticPathPolicyError::TooLong);
    }
    if path.as_bytes().get(1) == Some(&b':') {
        return Err(StaticPathPolicyError::Absolute);
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == ".")
    {
        return Err(StaticPathPolicyError::NotClean);
    }

    let path = PathBuf::from(path);
    validate_clean_relative_path(&path, false)?;
    Ok(path)
}

/// Decode and validate a URL path for static file resolution.
///
/// The returned path is decoded exactly once, matching filesystem lookup
/// semantics. A second decoded view is validated only as a security check so
/// callers are also protected when an HTTP stack has already decoded the path.
/// The second view is never returned or used for lookup: this preserves literal
/// percent filenames such as `100%25.svg`, and no filesystem layer decodes a
/// third time. `/` returns an empty path so the caller can apply its index rule.
pub fn normalize_static_request_path(raw_path: &str) -> Result<PathBuf, StaticPathPolicyError> {
    if raw_path.len() > MAX_STATIC_ASSET_URL_PATH_BYTES {
        return Err(StaticPathPolicyError::TooLong);
    }
    if raw_path.contains('\0') {
        return Err(StaticPathPolicyError::NotClean);
    }

    let decoded = percent_decode_once(raw_path.as_bytes());
    let second_view = percent_decode_once(&decoded);
    validate_decoded_request_path(&second_view)?;
    validate_decoded_request_path(&decoded)
}

fn validate_decoded_request_path(decoded: &[u8]) -> Result<PathBuf, StaticPathPolicyError> {
    if decoded.contains(&0) {
        return Err(StaticPathPolicyError::NotClean);
    }
    let decoded = std::str::from_utf8(decoded).map_err(|_| StaticPathPolicyError::NonUtf8)?;
    let relative = decoded.strip_prefix('/').unwrap_or(decoded);
    if relative.starts_with('/') || relative.contains('\\') {
        return Err(StaticPathPolicyError::NotClean);
    }
    let relative = relative.strip_suffix('/').unwrap_or(relative);
    if relative.is_empty() {
        return Ok(PathBuf::new());
    }
    if relative
        .split('/')
        .any(|component| component.is_empty() || component == ".")
    {
        return Err(StaticPathPolicyError::NotClean);
    }

    let path = PathBuf::from(relative);
    validate_static_artifact_path(&path)?;
    Ok(path)
}

/// Return whether a deployment path is unsafe to publish.
///
/// Dot-prefixed components are private by default. The standardized top-level
/// `.well-known` directory is the sole exception, allowing resources such as
/// `security.txt` and ACME challenge files. Sensitive files remain denied inside
/// that directory.
pub fn is_sensitive_static_path(path: &Path) -> bool {
    let mut components = path.components().enumerate();
    components.any(|(index, component)| {
        let Component::Normal(component) = component else {
            return true;
        };
        let Some(name) = component.to_str() else {
            return true;
        };
        let lower = name.to_ascii_lowercase();

        if lower.starts_with('.') && !(index == 0 && lower == ".well-known") {
            return true;
        }

        matches!(
            lower.as_str(),
            "id_rsa"
                | "id_dsa"
                | "id_ecdsa"
                | "id_ed25519"
                | "credentials"
                | "credentials.json"
                | "secrets.json"
                | "kubeconfig"
                | "client_secret.json"
                | "client_secrets.json"
                | "service-account.json"
                | "service-account-key.json"
                | "service_account.json"
                | "service_account_key.json"
                | "serviceaccountkey.json"
        ) || [
            ".env",
            ".pem",
            ".key",
            ".ppk",
            ".p12",
            ".pfx",
            ".jks",
            ".keystore",
            ".tfvars",
            ".map",
            ".sql",
            ".sqlite",
            ".sqlite3",
            ".bak",
            ".backup",
            ".orig",
            ".swp",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
            || lower.ends_with(".tfstate")
            || lower.contains(".tfstate.")
            || lower.contains(".env.")
    })
}

fn validate_clean_relative_path(
    path: &Path,
    allow_empty: bool,
) -> Result<(), StaticPathPolicyError> {
    if path.as_os_str().is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err(StaticPathPolicyError::Empty)
        };
    }
    if path.is_absolute() {
        return Err(StaticPathPolicyError::Absolute);
    }
    let mut component_count = 0;
    for component in path.components() {
        match component {
            Component::Normal(value) if value.to_str().is_some() => {
                component_count += 1;
                if component_count > MAX_STATIC_PATH_COMPONENTS {
                    return Err(StaticPathPolicyError::TooDeep);
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(StaticPathPolicyError::Absolute);
            }
            Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                return Err(StaticPathPolicyError::NotClean);
            }
        }
    }
    Ok(())
}

fn percent_decode_once(input: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(input.len());
    let mut offset = 0;
    while offset < input.len() {
        if input[offset] != b'%' {
            decoded.push(input[offset]);
            offset += 1;
            continue;
        }
        let Some((high, low)) = input
            .get(offset + 1)
            .and_then(|value| hex_value(*value))
            .zip(input.get(offset + 2).and_then(|value| hex_value(*value)))
        else {
            decoded.push(input[offset]);
            offset += 1;
            continue;
        };
        decoded.push((high << 4) | low);
        offset += 3;
    }
    decoded
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_normalization_accepts_ordinary_and_well_known_paths() {
        assert_eq!(
            normalize_static_request_path("/assets/app.js"),
            Ok(PathBuf::from("assets/app.js"))
        );
        assert_eq!(
            normalize_static_request_path("/.well-known/security.txt"),
            Ok(PathBuf::from(".well-known/security.txt"))
        );
        assert_eq!(normalize_static_request_path("/"), Ok(PathBuf::new()));
        assert_eq!(
            normalize_static_request_path("/docs/"),
            Ok(PathBuf::from("docs"))
        );
        assert_eq!(
            normalize_static_request_path("/assets/100%25.svg"),
            Ok(PathBuf::from("assets/100%.svg"))
        );
        assert_eq!(
            normalize_static_request_path("/assets/100%.svg"),
            Ok(PathBuf::from("assets/100%.svg"))
        );
    }

    #[test]
    fn request_normalization_rejects_raw_single_and_double_encoded_traversal() {
        for path in [
            "/../secret",
            "/%2e%2e/secret",
            "/%252e%252e/secret",
            "/assets%2f..%2fsecret",
            "/assets%252f..%252fsecret",
            "/..%5csecret",
            "/..%255csecret",
            "/assets/%00app.js",
        ] {
            assert!(
                normalize_static_request_path(path).is_err(),
                "{path} must be rejected"
            );
        }
    }

    #[test]
    fn request_normalization_rejects_oversized_paths_before_decoding() {
        let oversized = format!("/{}", "a".repeat(MAX_STATIC_ASSET_URL_PATH_BYTES));
        assert_eq!(
            normalize_static_request_path(&oversized),
            Err(StaticPathPolicyError::TooLong)
        );
    }

    #[test]
    fn sensitive_files_are_rejected_at_every_encoding_depth() {
        for path in [
            "/.git/config",
            "/%2egit/config",
            "/%252egit/config",
            "/.env",
            "/keys/server.pem",
            "/assets/app.js.map",
            "/.well-known/.env",
            "/production.env",
            "/terraform.tfstate",
            "/terraform.tfstate.backup",
            "/terraform.tfvars",
            "/keys/deploy.ppk",
            "/kubeconfig",
            "/client_secret.json",
            "/client_secrets.json",
            "/serviceAccountKey.json",
            "/service-account-key.json",
            "/service_account_key.json",
        ] {
            assert_eq!(
                normalize_static_request_path(path),
                Err(StaticPathPolicyError::Sensitive),
                "{path} must be rejected"
            );
        }

        for path in [
            "/client%5fsecret.json",
            "/client%255fsecret.json",
            "/terraform%2etfstate",
            "/terraform%252etfstate",
            "/production%2eenv",
            "/production%252eenv",
        ] {
            assert_eq!(
                normalize_static_request_path(path),
                Err(StaticPathPolicyError::Sensitive),
                "{path} must be rejected"
            );
        }
    }

    #[test]
    fn stored_static_directory_must_be_a_clean_relative_path() {
        assert_eq!(
            validate_static_dir("projects/site/production/deploy-1"),
            Ok(PathBuf::from("projects/site/production/deploy-1"))
        );
        for path in [
            "",
            "/tmp/site",
            "../site",
            "projects/../site",
            "projects//site",
            "projects/./site",
            "projects/site/",
            r"projects\site",
            r"C:\site",
        ] {
            assert!(
                validate_static_dir(path).is_err(),
                "{path} must be rejected"
            );
        }

        let oversized = "a".repeat(MAX_STATIC_ASSET_URL_PATH_BYTES + 1);
        assert_eq!(
            validate_static_dir(&oversized),
            Err(StaticPathPolicyError::TooLong)
        );
    }

    #[test]
    fn clean_relative_paths_have_a_shared_component_depth_limit() {
        let maximum = (0..MAX_STATIC_PATH_COMPONENTS)
            .map(|_| "a")
            .collect::<Vec<_>>()
            .join("/");
        let too_deep = format!("{maximum}/a");

        assert!(validate_static_dir(&maximum).is_ok());
        assert_eq!(
            validate_static_dir(&too_deep),
            Err(StaticPathPolicyError::TooDeep)
        );
        assert_eq!(
            validate_static_artifact_path(Path::new(&too_deep)),
            Err(StaticPathPolicyError::TooDeep)
        );
    }

    #[test]
    fn artifact_policy_rejects_sensitive_names_but_allows_well_known() {
        assert!(validate_static_artifact_path(Path::new("index.html")).is_ok());
        assert!(validate_static_artifact_path(Path::new(".well-known/security.txt")).is_ok());
        assert_eq!(
            validate_static_artifact_path(Path::new("nested/.env.production")),
            Err(StaticPathPolicyError::Sensitive)
        );
        assert_eq!(
            validate_static_artifact_path(Path::new("source/app.ts.map")),
            Err(StaticPathPolicyError::Sensitive)
        );
        for path in [
            "production.env",
            "terraform.tfstate",
            "terraform.tfstate.123",
            "terraform.tfstate.backup",
            "terraform.tfvars",
            "keys/deploy.ppk",
            "kubeconfig",
            "client_secret.json",
            "client_secrets.json",
            "serviceAccountKey.json",
            "service-account.json",
            "service-account-key.json",
            "service_account.json",
            "service_account_key.json",
        ] {
            assert_eq!(
                validate_static_artifact_path(Path::new(path)),
                Err(StaticPathPolicyError::Sensitive),
                "{path} must be rejected"
            );
        }
    }
}
