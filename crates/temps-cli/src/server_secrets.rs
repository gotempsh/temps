// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The installation secrets as the installed `temps serve` sees them.
//!
//! An admin command that decrypts or encrypts what the server stores
//! (`domain import`, `backfill cloud-telemetry`, `network setup-multi-node`)
//! must use the server's encryption key, not one of its own. When it uses
//! the installed server's database, or may be using it (a service
//! definition was found but its database URL could not be read), the key is
//! read the way that server reads it: injected through the service's
//! environment, else from the service's data directory. It is never created
//! then, since a new key could not decrypt what the server already stored,
//! and the server would not be able to read what the command writes with
//! it.
//!
//! If this shell sets any of the secret variables, its settings replace the
//! service's (the operator is testing an injected secret), and every message
//! says so.

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

/// The secret variables this shell sets.
pub(crate) fn shell_secret_variables() -> Vec<&'static str> {
    SECRET_VARIABLES
        .into_iter()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

/// Which environment the secrets are read with: this shell's when it sets
/// any of them (the operator is testing that), else the installed service's,
/// which a shell does not inherit.
pub(crate) enum SecretsEnvironment<'a> {
    /// This shell's, which sets these variables.
    Shell(Vec<&'static str>),
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
        Self::choose_with(service, shell_secret_variables())
    }

    /// The choice when this shell sets `shell_variables`.
    pub(crate) fn choose_with(
        service: Option<&'a ServiceDefinition>,
        shell_variables: Vec<&'static str>,
    ) -> Self {
        if !shell_variables.is_empty() {
            return SecretsEnvironment::Shell(shell_variables);
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
            SecretsEnvironment::Shell(_) => std::env::var(name).ok(),
            SecretsEnvironment::Service(service) => service.environment.get(name).cloned(),
            SecretsEnvironment::Unknown { .. } | SecretsEnvironment::Empty => None,
        }
    }

    /// Which environment was used, for messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            SecretsEnvironment::Shell(variables) => format!(
                "this shell sets {}, so its environment was used and the installed service's \
                 secret settings were not",
                variables.join(", ")
            ),
            SecretsEnvironment::Service(service) => {
                format!("read with the environment of {}", service.path.display())
            }
            SecretsEnvironment::Unknown { service, reason } => format!(
                "the environment of {} could not be read completely: {reason}",
                service.path.display()
            ),
            SecretsEnvironment::Empty => {
                "neither this shell nor the installed service injects one".to_string()
            }
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
        "The installed service {} was found, but the database URL it gives the server could \
         not be read ({reason}), so this command cannot tell whether it uses the server's \
         database, or which data directory holds the server's encryption key. Run it as a user \
         that can read the service's files (usually with sudo), or pass --data-dir with the \
         server's data directory",
        definition.display()
    )]
    ServerUnknown { definition: PathBuf, reason: String },
    #[error(
        "This command uses (or may use) the database of the installed service {}, but the \
         environment it gives the server could not be read completely ({reason}), so the \
         encryption key the server uses is unknown. Run the command as a user that can read \
         it (usually with sudo), or set {} or {} in this shell, which then replaces all of the \
         service's secret settings",
        definition.display(),
        temps_config::ENCRYPTION_KEY_ENV,
        temps_config::ENCRYPTION_KEY_FILE_ENV
    )]
    EnvironmentUnknown { definition: PathBuf, reason: String },
    #[error(
        "The server's encryption key {} does not exist ({environment}), so nothing can be \
         encrypted for the installed service {}. If this is the server's data directory and \
         its key is not injected, start `temps serve` once so it creates the key; otherwise \
         pass --data-dir with the server's data directory. A key created by this command \
         could not be read by the server",
        path.display(),
        definition.display()
    )]
    KeyNotCreated {
        definition: PathBuf,
        path: PathBuf,
        environment: String,
    },
    #[error(
        "Could not read the encryption key for data directory {} ({environment}): {source}",
        data_dir.display()
    )]
    Secrets {
        data_dir: PathBuf,
        environment: String,
        #[source]
        source: Box<temps_config::ConfigServiceError>,
    },
}

/// How the database a command uses relates to the installed server.
enum InstalledServer<'a> {
    /// The installed server's own database.
    Serving(&'a ServiceDefinition),
    /// A definition was found, but the URL it gives the server could not be
    /// read, so it may be the server's database.
    Unknown {
        service: &'a ServiceDefinition,
        reason: &'a str,
    },
    /// Another database, or no installed server.
    Other,
}

fn installed_server(database: &ResolvedDatabaseUrl) -> InstalledServer<'_> {
    let Some(service) = database.service() else {
        return InstalledServer::Other;
    };
    match &service.database_url {
        Ok(Some((url, _))) if url == database.url() => InstalledServer::Serving(service),
        Ok(_) => InstalledServer::Other,
        Err(reason) => InstalledServer::Unknown { service, reason },
    }
}

/// The data directory of the server whose database `database` is:
/// `Ok(None)` when it is not the installed server's (the caller's own
/// default applies), an error when it is (or may be) but the service does
/// not say which directory the server uses.
pub(crate) fn service_data_dir(
    database: &ResolvedDatabaseUrl,
) -> Result<Option<PathBuf>, ServerSecretsError> {
    match installed_server(database) {
        InstalledServer::Other => Ok(None),
        InstalledServer::Unknown { service, reason } => Err(ServerSecretsError::ServerUnknown {
            definition: service.path.clone(),
            reason: reason.to_string(),
        }),
        InstalledServer::Serving(service) => match &service.data_dir {
            Ok((dir, _)) => Ok(Some(dir.clone())),
            Err(reason) => Err(ServerSecretsError::DataDirUnknown {
                definition: service.path.clone(),
                reason: reason.clone(),
            }),
        },
    }
}

/// The encryption key the server uses with `data_dir`. With the installed
/// server's database, or one that may be it, it is read as that server
/// reads it and never created; otherwise it is resolved as `temps serve`
/// would on this host, creating a missing one.
pub(crate) fn encryption_key(
    data_dir: &Path,
    database: &ResolvedDatabaseUrl,
) -> Result<String, ServerSecretsError> {
    encryption_key_in(data_dir, database, shell_secret_variables())
}

fn encryption_key_in(
    data_dir: &Path,
    database: &ResolvedDatabaseUrl,
    shell_variables: Vec<&'static str>,
) -> Result<String, ServerSecretsError> {
    match installed_server(database) {
        InstalledServer::Serving(service) | InstalledServer::Unknown { service, .. } => {
            service_encryption_key(
                data_dir,
                service,
                SecretsEnvironment::choose_with(Some(service), shell_variables),
            )
        }
        InstalledServer::Other => temps_config::resolve_installation_secrets(data_dir)
            .map(|secrets| secrets.encryption_key)
            .map_err(|source| ServerSecretsError::Secrets {
                data_dir: data_dir.to_path_buf(),
                environment: "as `temps serve` would read it on this host".to_string(),
                source: Box::new(source),
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
                environment: environment.describe(),
            })
        }
        Err(source) => Err(ServerSecretsError::Secrets {
            data_dir: data_dir.to_path_buf(),
            environment: environment.describe(),
            source: Box::new(source),
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
        crate::service_definition::test_support::write_trusted(path, contents).unwrap();
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
            &format!(
                "[Service]\nUser=temps-test-no-such-user\n\
                 ExecStart=/usr/bin/temps serve --database-url={URL}\n"
            ),
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
    fn a_service_whose_url_is_unknown_is_never_given_a_new_key() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let env_file = root.path().join("temps.env");
        write(&env_file, &format!("TEMPS_DATABASE_URL={URL}\n"));
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&env_file).is_ok() {
            // Running as root: every file is readable.
            return;
        }
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironmentFile={}\nExecStart=/usr/bin/temps serve\n",
                env_file.display()
            ),
        );
        // The operator passes the server's URL; whether it is the server's
        // cannot be told, since the unit's URL is in the unreadable file.
        let database = resolve(&unit, Some(URL));
        let error = service_data_dir(&database).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::ServerUnknown { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("--data-dir"), "{error}");

        let data = root.path().join("explicit-data");
        std::fs::create_dir_all(&data).unwrap();
        let error = encryption_key_in(&data, &database, Vec::new()).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::EnvironmentUnknown { .. }),
            "{error}"
        );
        assert!(
            !data.join(temps_config::ENCRYPTION_KEY_FILE).exists(),
            "a key was created"
        );
    }

    #[test]
    fn a_missing_key_says_when_this_shell_replaced_the_services_secrets() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment={}=/etc/temps/encryption-key\n\
                 ExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                temps_config::ENCRYPTION_KEY_FILE_ENV,
                data.display()
            ),
        );
        let database = resolve(&unit, None);
        let service = database.serving_service().unwrap();
        let error = service_encryption_key(
            &data,
            service,
            SecretsEnvironment::choose_with(Some(service), vec![temps_config::AUTH_SECRET_ENV]),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(&format!(
                "this shell sets {}",
                temps_config::AUTH_SECRET_ENV
            )) && message.contains("secret settings were not"),
            "{message}"
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
