// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::TryRng;

use crate::service::{AUTH_SECRET_FILE, ENCRYPTION_KEY_FILE};
use crate::ConfigServiceError;

pub const STATELESS_ENV: &str = "TEMPS_STATELESS";
pub const AUTH_SECRET_ENV: &str = "TEMPS_AUTH_SECRET";
pub const AUTH_SECRET_FILE_ENV: &str = "TEMPS_AUTH_SECRET_FILE";
pub const ENCRYPTION_KEY_ENV: &str = "TEMPS_ENCRYPTION_KEY";
pub const ENCRYPTION_KEY_FILE_ENV: &str = "TEMPS_ENCRYPTION_KEY_FILE";

#[derive(Clone, PartialEq, Eq)]
pub struct InstallationSecrets {
    pub auth_secret: String,
    pub encryption_key: String,
}

impl std::fmt::Debug for InstallationSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallationSecrets")
            .field("auth_secret", &"[REDACTED]")
            .field("encryption_key", &"[REDACTED]")
            .finish()
    }
}

pub fn stateless_mode_enabled() -> Result<bool, ConfigServiceError> {
    parse_stateless(read_environment(STATELESS_ENV)?.as_deref())
}

pub fn resolve_installation_secrets(
    data_dir: &Path,
) -> Result<InstallationSecrets, ConfigServiceError> {
    let values = [
        STATELESS_ENV,
        AUTH_SECRET_ENV,
        AUTH_SECRET_FILE_ENV,
        ENCRYPTION_KEY_ENV,
        ENCRYPTION_KEY_FILE_ENV,
    ]
    .into_iter()
    .map(|name| read_environment(name).map(|value| (name, value)))
    .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
    resolve_installation_secrets_with(data_dir, |name| values.get(name).cloned().flatten())
}

fn read_environment(name: &'static str) -> Result<Option<String>, ConfigServiceError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigServiceError::InvalidEnvironmentValue {
                variable: name,
                details: "value is not valid Unicode".to_string(),
            })
        }
    }
}

fn resolve_installation_secrets_with(
    data_dir: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<InstallationSecrets, ConfigServiceError> {
    let stateless = parse_stateless(env(STATELESS_ENV).as_deref())?;
    let auth_secret = resolve_secret(
        data_dir,
        AUTH_SECRET_FILE,
        AUTH_SECRET_ENV,
        AUTH_SECRET_FILE_ENV,
        stateless,
        &env,
        validate_auth_secret,
    )?;
    let encryption_key = resolve_secret(
        data_dir,
        ENCRYPTION_KEY_FILE,
        ENCRYPTION_KEY_ENV,
        ENCRYPTION_KEY_FILE_ENV,
        stateless,
        &env,
        validate_encryption_key,
    )?;
    Ok(InstallationSecrets {
        auth_secret,
        encryption_key,
    })
}

fn parse_stateless(value: Option<&str>) -> Result<bool, ConfigServiceError> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(value) => Err(ConfigServiceError::InvalidEnvironmentValue {
            variable: STATELESS_ENV,
            details: format!("expected 'true' or 'false', received '{value}'"),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_secret(
    data_dir: &Path,
    local_filename: &'static str,
    value_env: &'static str,
    file_env: &'static str,
    stateless: bool,
    env: &impl Fn(&str) -> Option<String>,
    validate: fn(&str, &'static str) -> Result<(), ConfigServiceError>,
) -> Result<String, ConfigServiceError> {
    let direct = env(value_env);
    let file = env(file_env);
    if direct.is_some() && file.is_some() {
        return Err(ConfigServiceError::ConflictingSecretSources {
            value_variable: value_env,
            file_variable: file_env,
        });
    }

    if let Some(value) = direct {
        validate(&value, value_env)?;
        return Ok(value);
    }
    if let Some(path) = file {
        if path.trim().is_empty() {
            return Err(ConfigServiceError::InvalidEnvironmentValue {
                variable: file_env,
                details: "secret file path cannot be empty".to_string(),
            });
        }
        let value = fs::read_to_string(&path)
            .map_err(|source| ConfigServiceError::SecretFileRead {
                variable: file_env,
                path: PathBuf::from(&path),
                source,
            })?
            .trim()
            .to_string();
        validate(&value, file_env)?;
        return Ok(value);
    }
    if stateless {
        return Err(ConfigServiceError::MissingStatelessSecret {
            value_variable: value_env,
            file_variable: file_env,
        });
    }

    load_or_create_local_secret(data_dir, local_filename)
}

fn load_or_create_local_secret(
    data_dir: &Path,
    filename: &'static str,
) -> Result<String, ConfigServiceError> {
    let path = data_dir.join(filename);
    match fs::read_to_string(&path) {
        Ok(value) => return Ok(value.trim().to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ConfigServiceError::LocalSecretRead {
                path: path.clone(),
                source,
            })
        }
    }

    fs::create_dir_all(data_dir).map_err(|source| ConfigServiceError::LocalSecretWrite {
        path: path.clone(),
        source,
    })?;
    let mut bytes = [0_u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| ConfigServiceError::RandomnessFailed {
            operation: format!("creating persisted installation secret {filename}"),
            reason: error.to_string(),
        })?;
    let value = hex::encode(bytes);
    if write_new_secret(&path, &value)? {
        Ok(value)
    } else {
        fs::read_to_string(&path)
            .map(|existing| existing.trim().to_string())
            .map_err(|source| ConfigServiceError::LocalSecretRead { path, source })
    }
}

fn write_new_secret(path: &Path, value: &str) -> Result<bool, ConfigServiceError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(value.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|source| ConfigServiceError::LocalSecretWrite {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(source) => Err(ConfigServiceError::LocalSecretWrite {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn validate_auth_secret(value: &str, origin: &'static str) -> Result<(), ConfigServiceError> {
    if value.len() < 32 {
        return Err(ConfigServiceError::InvalidInjectedSecret {
            origin,
            details: "auth secret must contain at least 32 bytes".to_string(),
        });
    }
    Ok(())
}

fn validate_encryption_key(value: &str, origin: &'static str) -> Result<(), ConfigServiceError> {
    temps_core::EncryptionService::new(value).map_err(|error| {
        ConfigServiceError::InvalidInjectedSecret {
            origin,
            details: error.to_string(),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(
        data_dir: &Path,
        values: &[(&str, &str)],
    ) -> Result<InstallationSecrets, ConfigServiceError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        resolve_installation_secrets_with(data_dir, |name| values.get(name).cloned())
    }

    #[test]
    fn stateless_mode_requires_both_secrets() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let error = resolve(dir.path(), &[(STATELESS_ENV, "true")])
            .expect_err("missing stateless secrets must fail");
        assert!(matches!(
            error,
            ConfigServiceError::MissingStatelessSecret { .. }
        ));
        assert!(!dir.path().join(AUTH_SECRET_FILE).exists());
    }

    #[test]
    fn injected_values_work_in_stateless_mode_without_local_files() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let secret = "a".repeat(64);
        let resolved = resolve(
            dir.path(),
            &[
                (STATELESS_ENV, "true"),
                (AUTH_SECRET_ENV, &secret),
                (ENCRYPTION_KEY_ENV, &secret),
            ],
        )
        .expect("valid injected secrets");
        assert_eq!(resolved.auth_secret, secret);
        assert!(!dir.path().join(AUTH_SECRET_FILE).exists());
        assert!(!dir.path().join(ENCRYPTION_KEY_FILE).exists());
    }

    #[test]
    fn conflicting_secret_sources_fail_closed() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let error = resolve(
            dir.path(),
            &[
                (AUTH_SECRET_ENV, &"a".repeat(64)),
                (AUTH_SECRET_FILE_ENV, "/run/secrets/auth"),
            ],
        )
        .expect_err("conflicting sources must fail");
        assert!(matches!(
            error,
            ConfigServiceError::ConflictingSecretSources { .. }
        ));
    }

    #[test]
    fn file_injection_trims_a_trailing_newline() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let auth_path = dir.path().join("injected-auth");
        let encryption_path = dir.path().join("injected-encryption");
        fs::write(&auth_path, format!("{}\n", "b".repeat(64))).expect("write auth secret");
        fs::write(&encryption_path, format!("{}\n", "c".repeat(64))).expect("write encryption key");
        let resolved = resolve(
            dir.path(),
            &[
                (STATELESS_ENV, "true"),
                (AUTH_SECRET_FILE_ENV, &auth_path.to_string_lossy()),
                (ENCRYPTION_KEY_FILE_ENV, &encryption_path.to_string_lossy()),
            ],
        )
        .expect("file injected secrets");
        assert_eq!(resolved.auth_secret, "b".repeat(64));
        assert_eq!(resolved.encryption_key, "c".repeat(64));
    }

    #[test]
    fn local_mode_persists_and_reuses_generated_secrets() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let first = resolve(dir.path(), &[]).expect("generate local secrets");
        let second = resolve(dir.path(), &[]).expect("reload local secrets");
        assert_eq!(first, second);
        assert_eq!(first.auth_secret.len(), 64);
        assert_eq!(first.encryption_key.len(), 64);
    }

    #[test]
    fn invalid_stateless_value_is_rejected() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let error =
            resolve(dir.path(), &[(STATELESS_ENV, "1")]).expect_err("ambiguous opt-in must fail");
        assert!(matches!(
            error,
            ConfigServiceError::InvalidEnvironmentValue { .. }
        ));
    }
}
