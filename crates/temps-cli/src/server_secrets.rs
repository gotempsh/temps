// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The installation secrets as the installed `temps serve` sees them.
//!
//! An admin command that decrypts or encrypts what the server stores
//! (`domain import`, `backfill cloud-telemetry`, `network setup-multi-node`)
//! must use the server's encryption key, not one of its own. When it uses
//! the installed server's database, the key is read the way that server
//! reads it: injected through the service's environment, else from the
//! service's data directory. It is never created there, since a new key
//! could not decrypt what the server already stored, and the server would
//! not be able to read what the command writes with it.

use std::path::{Path, PathBuf};

use crate::database_url_source::ResolvedDatabaseUrl;
use crate::service_definition::ServiceDefinition;

/// The variables `ServerConfig::new` reads the installation secrets from.
pub(crate) const SECRET_VARIABLES: [&str; 5] = [
    temps_config::STATELESS_ENV,
    temps_config::AUTH_SECRET_ENV,
    temps_config::AUTH_SECRET_FILE_ENV,
    temps_config::ENCRYPTION_KEY_ENV,
    temps_config::ENCRYPTION_KEY_FILE_ENV,
];

/// Which environment the secrets are read with: this shell's when it injects
/// any of them (the operator is testing that), else the installed service's,
/// which a shell does not inherit.
pub(crate) enum SecretsEnvironment<'a> {
    Shell,
    Service(&'a ServiceDefinition),
    /// The service's environment could not be read completely, so what it
    /// injects is unknown.
    Unknown {
        service: &'a ServiceDefinition,
        reason: String,
    },
    Empty,
}

impl<'a> SecretsEnvironment<'a> {
    pub(crate) fn choose(service: Option<&'a ServiceDefinition>) -> Self {
        if SECRET_VARIABLES
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            return SecretsEnvironment::Shell;
        }
        Self::of_service(service)
    }

    /// The choice when this shell injects none of the secrets.
    pub(crate) fn of_service(service: Option<&'a ServiceDefinition>) -> Self {
        let Some(service) = service else {
            return SecretsEnvironment::Empty;
        };
        if let Some(reason) = service.incomplete_environment() {
            return SecretsEnvironment::Unknown { service, reason };
        }
        if SECRET_VARIABLES
            .iter()
            .any(|name| service.environment.contains_key(*name))
        {
            SecretsEnvironment::Service(service)
        } else {
            SecretsEnvironment::Empty
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        match self {
            SecretsEnvironment::Shell => std::env::var(name).ok(),
            SecretsEnvironment::Service(service) => service.environment.get(name).cloned(),
            SecretsEnvironment::Unknown { .. } | SecretsEnvironment::Empty => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServerSecretsError {
    #[error(
        "This command uses the database of the installed service {}, but the server's data \
         directory, which holds its encryption key, is unknown: {reason}. Pass --data-dir \
         with the server's data directory",
        definition.display()
    )]
    DataDirUnknown { definition: PathBuf, reason: String },
    #[error(
        "This command uses the database of the installed service {}, but the environment it \
         gives the server could not be read completely ({reason}), so the encryption key the \
         server uses is unknown. Run the command as a user that can read it (usually with \
         sudo), or pass the key with {} or {}",
        definition.display(),
        temps_config::ENCRYPTION_KEY_ENV,
        temps_config::ENCRYPTION_KEY_FILE_ENV
    )]
    EnvironmentUnknown { definition: PathBuf, reason: String },
    #[error(
        "The server's encryption key {} does not exist, so nothing can be encrypted for the \
         installed service {}. If this is the server's data directory, start `temps serve` \
         once so it creates the key; otherwise pass --data-dir with the server's data \
         directory. A key created by this command could not be read by the server",
        path.display(),
        definition.display()
    )]
    KeyNotCreated { definition: PathBuf, path: PathBuf },
    #[error("Could not read the encryption key for data directory {}: {source}", data_dir.display())]
    Secrets {
        data_dir: PathBuf,
        #[source]
        source: temps_config::ConfigServiceError,
    },
}

/// The data directory of the server whose database `database` is:
/// `Ok(None)` when it is not the installed server's (the caller's own
/// default applies), an error when it is but the service does not say which
/// directory the server uses.
pub(crate) fn service_data_dir(
    database: &ResolvedDatabaseUrl,
) -> Result<Option<PathBuf>, ServerSecretsError> {
    let Some(service) = database.serving_service() else {
        return Ok(None);
    };
    match &service.data_dir {
        Ok((dir, _)) => Ok(Some(dir.clone())),
        Err(reason) => Err(ServerSecretsError::DataDirUnknown {
            definition: service.path.clone(),
            reason: reason.clone(),
        }),
    }
}

/// The encryption key the server uses with `data_dir`. With the installed
/// server's own database it is read as that server reads it, and never
/// created; otherwise it is resolved as `temps serve` would on this host,
/// creating a missing one.
pub(crate) fn encryption_key(
    data_dir: &Path,
    database: &ResolvedDatabaseUrl,
) -> Result<String, ServerSecretsError> {
    match database.serving_service() {
        Some(service) => {
            service_encryption_key(data_dir, service, SecretsEnvironment::choose(Some(service)))
        }
        None => temps_config::resolve_installation_secrets(data_dir)
            .map(|secrets| secrets.encryption_key)
            .map_err(|source| ServerSecretsError::Secrets {
                data_dir: data_dir.to_path_buf(),
                source,
            }),
    }
}

fn service_encryption_key(
    data_dir: &Path,
    service: &ServiceDefinition,
    environment: SecretsEnvironment<'_>,
) -> Result<String, ServerSecretsError> {
    if let SecretsEnvironment::Unknown { reason, .. } = &environment {
        return Err(ServerSecretsError::EnvironmentUnknown {
            definition: service.path.clone(),
            reason: reason.clone(),
        });
    }
    let inspection =
        temps_config::inspect_installation_secrets_with(data_dir, |name| environment.get(name));
    match inspection.encryption_key {
        Ok(temps_config::InspectedSecret::Found { value, .. }) => Ok(value),
        Ok(temps_config::InspectedSecret::NotCreatedYet { path }) => {
            Err(ServerSecretsError::KeyNotCreated {
                definition: service.path.clone(),
                path,
            })
        }
        Err(source) => Err(ServerSecretsError::Secrets {
            data_dir: data_dir.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database_url_source::{resolve_database_url_in, UrlDiscovery};
    use crate::service_definition::ServiceLocations;

    const URL: &str = "postgresql://temps:s3cr3t-pw@127.0.0.1:5432/temps";
    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn resolve(unit: &Path, flag: Option<&str>) -> ResolvedDatabaseUrl {
        resolve_database_url_in(
            flag,
            UrlDiscovery::InstalledService,
            &ServiceLocations {
                systemd_unit: Some(unit.to_path_buf()),
                launchd_agent: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn the_service_data_dir_is_required_with_the_services_database() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!("[Service]\nExecStart=/usr/bin/temps serve --database-url={URL}\n"),
        );
        let error = service_data_dir(&resolve(&unit, None)).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::DataDirUnknown { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("--data-dir"), "{error}");

        // A different database is not the server's: the caller's default.
        assert_eq!(
            service_data_dir(&resolve(&unit, Some("postgresql://elsewhere/db"))).unwrap(),
            None
        );

        let data = root.path().join("data");
        write(
            &unit,
            &format!(
                "[Service]\nExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                data.display()
            ),
        );
        assert_eq!(service_data_dir(&resolve(&unit, None)).unwrap(), Some(data));
    }

    #[test]
    fn the_key_is_never_created_for_the_installed_server() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                data.display()
            ),
        );
        let database = resolve(&unit, None);
        let service = database.serving_service().unwrap();

        let error = service_encryption_key(
            &data,
            service,
            SecretsEnvironment::of_service(Some(service)),
        )
        .unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::KeyNotCreated { .. }),
            "{error}"
        );
        assert!(
            !data.join(temps_config::ENCRYPTION_KEY_FILE).exists(),
            "a key was created"
        );

        write(&data.join(temps_config::ENCRYPTION_KEY_FILE), KEY);
        assert_eq!(
            service_encryption_key(
                &data,
                service,
                SecretsEnvironment::of_service(Some(service))
            )
            .unwrap(),
            KEY
        );
    }

    #[test]
    fn the_key_the_service_injects_wins_over_the_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let key_file = root.path().join("injected-key");
        write(&key_file, KEY);
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment={}={}\n\
                 ExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                temps_config::ENCRYPTION_KEY_FILE_ENV,
                key_file.display(),
                data.display()
            ),
        );
        let database = resolve(&unit, None);
        let service = database.serving_service().unwrap();
        assert_eq!(
            service_encryption_key(
                &data,
                service,
                SecretsEnvironment::of_service(Some(service))
            )
            .unwrap(),
            KEY
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_service_environment_leaves_the_key_unknown() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        write(&data.join(temps_config::ENCRYPTION_KEY_FILE), KEY);
        let env_file = root.path().join("temps.env");
        write(&env_file, "TEMPS_ENCRYPTION_KEY=elsewhere\n");
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&env_file).is_ok() {
            // Running as root: every file is readable.
            return;
        }
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironmentFile={}\n\
                 ExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                env_file.display(),
                data.display()
            ),
        );
        let database = resolve(&unit, None);
        let service = database.serving_service().unwrap();
        let error = service_encryption_key(
            &data,
            service,
            SecretsEnvironment::of_service(Some(service)),
        )
        .unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::EnvironmentUnknown { .. }),
            "{error}"
        );
        assert!(!error.to_string().contains(KEY), "{error}");
    }
}
