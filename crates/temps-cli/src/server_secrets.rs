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
//! service's (the operator is testing an injected secret). The command
//! prints which key it used, and from which environment, every time.

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
         database, nor which encryption key to use. Run it as a user that can read the \
         service's files (usually with sudo). Or set {} or {} in this shell to the key of the \
         installation the database belongs to, and pass --data-dir with that installation's \
         data directory",
        definition.display(),
        temps_config::ENCRYPTION_KEY_ENV,
        temps_config::ENCRYPTION_KEY_FILE_ENV
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
        Ok(Some((url, _))) if crate::database_url_source::same_database(url, database.url()) => {
            InstalledServer::Serving(service)
        }
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

/// An encryption key, and for the installed server's where it was read from.
pub(crate) struct ServerKey {
    value: String,
    notice: Option<String>,
    /// Read from the data directory's `encryption_key`, not injected.
    from_data_dir: bool,
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerKey")
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

impl ServerKey {
    /// Which key was used and from which environment, never the key.
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Print [`Self::notice`] to stderr, so the key a command encrypts with
    /// is never chosen silently, and scripted stdout is unchanged.
    pub(crate) fn announce(&self) {
        if let Some(notice) = self.notice() {
            eprintln!("{notice}");
        }
    }

    pub(crate) fn into_value(self) -> String {
        self.value
    }
}

/// The encryption key the server uses with `data_dir`. With the installed
/// server's database it is read as that server reads it and never created.
/// With one that may be the server's (its URL could not be read) only a key
/// this shell sets is used, since which key is right cannot be told.
/// Otherwise it is resolved as `temps serve` would on this host, creating a
/// missing one.
pub(crate) fn encryption_key(
    data_dir: &Path,
    database: &ResolvedDatabaseUrl,
) -> Result<ServerKey, ServerSecretsError> {
    encryption_key_in(data_dir, database, shell_secret_variables())
}

fn encryption_key_in(
    data_dir: &Path,
    database: &ResolvedDatabaseUrl,
    shell_variables: Vec<&'static str>,
) -> Result<ServerKey, ServerSecretsError> {
    match installed_server(database) {
        InstalledServer::Serving(service) => service_encryption_key(
            data_dir,
            service,
            SecretsEnvironment::choose_with(Some(service), shell_variables),
        ),
        InstalledServer::Unknown { service, reason } => {
            let unknown = || ServerSecretsError::ServerUnknown {
                definition: service.path.clone(),
                reason: reason.to_string(),
            };
            match SecretsEnvironment::choose_with(Some(service), shell_variables) {
                // Only a key the shell itself sets: one that sets another
                // secret variable still leaves the key to the data
                // directory, which says nothing about whose it is.
                environment @ SecretsEnvironment::Shell(_) => {
                    let key = service_encryption_key(data_dir, service, environment)?;
                    if key.from_data_dir {
                        Err(unknown())
                    } else {
                        Ok(key)
                    }
                }
                SecretsEnvironment::Service(_)
                | SecretsEnvironment::Unknown { .. }
                | SecretsEnvironment::Empty => Err(unknown()),
            }
        }
        InstalledServer::Other => temps_config::resolve_installation_secrets(data_dir)
            .map(|secrets| ServerKey {
                value: secrets.encryption_key,
                notice: None,
                from_data_dir: true,
            })
            .map_err(|source| ServerSecretsError::Secrets {
                data_dir: data_dir.to_path_buf(),
                environment: "as `temps serve` would read it on this host".to_string(),
                source: Box::new(source),
            }),
    }
}

fn describe_key_source(source: &temps_config::SecretSource) -> String {
    match source {
        temps_config::SecretSource::Environment { variable } => variable.to_string(),
        temps_config::SecretSource::InjectedFile { variable, path } => {
            format!("{} (named by {variable})", path.display())
        }
        temps_config::SecretSource::DataDir { path } => path.display().to_string(),
    }
}

fn service_encryption_key(
    data_dir: &Path,
    service: &ServiceDefinition,
    environment: SecretsEnvironment<'_>,
) -> Result<ServerKey, ServerSecretsError> {
    if let SecretsEnvironment::Unknown { reason, .. } = &environment {
        return Err(ServerSecretsError::EnvironmentUnknown {
            definition: service.path.clone(),
            reason: reason.clone(),
        });
    }
    let inspection =
        temps_config::inspect_installation_secrets_with(data_dir, |name| environment.get(name));
    match inspection.encryption_key {
        Ok(temps_config::InspectedSecret::Found { value, source }) => Ok(ServerKey {
            value,
            notice: Some(format!(
                "Using the encryption key {} ({}).",
                describe_key_source(&source),
                environment.describe()
            )),
            from_data_dir: matches!(source, temps_config::SecretSource::DataDir { .. }),
        }),
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
            .unwrap()
            .into_value(),
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
            .unwrap()
            .into_value(),
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
        // The remedies it names are the ones that work here.
        let message = error.to_string();
        assert!(
            message.contains("sudo")
                && message.contains(temps_config::ENCRYPTION_KEY_ENV)
                && message.contains("--data-dir"),
            "{message}"
        );

        // --data-dir alone does not tell which key is the server's: no key
        // is created, and none found there is used.
        let data = root.path().join("explicit-data");
        std::fs::create_dir_all(&data).unwrap();
        let error = encryption_key_in(&data, &database, Vec::new()).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::ServerUnknown { .. }),
            "{error}"
        );
        assert!(
            !data.join(temps_config::ENCRYPTION_KEY_FILE).exists(),
            "a key was created"
        );
        write(&data.join(temps_config::ENCRYPTION_KEY_FILE), KEY);
        assert!(encryption_key_in(&data, &database, Vec::new()).is_err());

        // Nor when this shell sets a secret variable other than the key's:
        // the key would still come from --data-dir.
        for variable in [
            temps_config::AUTH_SECRET_ENV,
            temps_config::AUTH_SECRET_FILE_ENV,
            temps_config::STATELESS_ENV,
        ] {
            if std::env::var_os(temps_config::ENCRYPTION_KEY_ENV).is_some()
                || std::env::var_os(temps_config::ENCRYPTION_KEY_FILE_ENV).is_some()
            {
                // This test process injects a key itself; nothing to check.
                break;
            }
            let error = encryption_key_in(&data, &database, vec![variable]).unwrap_err();
            assert!(
                matches!(error, ServerSecretsError::ServerUnknown { .. }),
                "{variable}: {error}"
            );
            assert!(!error.to_string().contains(KEY), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_services_injected_key_is_not_used_for_a_database_that_may_be_another() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        // The service's URL is in a file this user cannot read; the key it
        // injects is readable.
        let url_file = root.path().join("database-url");
        write(&url_file, &format!("{URL}\n"));
        std::fs::set_permissions(&url_file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&url_file).is_ok() {
            // Running as root: every file is readable.
            return;
        }
        let service_key = root.path().join("service-key");
        write(&service_key, KEY);
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE={} {}={}\n\
                 ExecStart=/usr/bin/temps serve\n",
                url_file.display(),
                temps_config::ENCRYPTION_KEY_FILE_ENV,
                service_key.display()
            ),
        );
        // Another installation, named explicitly with its own data dir.
        let database = resolve(&unit, Some("postgresql://other-install/temps"));
        let other = root.path().join("other");
        write(&other.join(temps_config::ENCRYPTION_KEY_FILE), "other-key");
        let error = encryption_key_in(&other, &database, Vec::new()).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::ServerUnknown { .. }),
            "{error}"
        );
        assert!(!error.to_string().contains(KEY), "{error}");
    }

    #[test]
    fn the_key_used_is_announced_without_its_value() {
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
        let key = service_encryption_key(
            &data,
            service,
            SecretsEnvironment::of_service(Some(service)),
        )
        .unwrap();
        let notice = key.notice().unwrap();
        assert!(
            notice.contains(&key_file.display().to_string())
                && notice.contains(temps_config::ENCRYPTION_KEY_FILE_ENV)
                && notice.contains("temps.service"),
            "{notice}"
        );
        assert!(!notice.contains(KEY), "{notice}");
        assert!(!format!("{key:?}").contains(KEY));

        // A shell override is named in the notice of a key that was found.
        write(&data.join(temps_config::ENCRYPTION_KEY_FILE), KEY);
        let key = service_encryption_key(
            &data,
            service,
            SecretsEnvironment::choose_with(Some(service), vec![temps_config::AUTH_SECRET_ENV]),
        )
        .unwrap();
        let notice = key.notice().unwrap();
        assert!(
            notice.contains(&format!(
                "this shell sets {}",
                temps_config::AUTH_SECRET_ENV
            )) && notice.contains("secret settings were not"),
            "{notice}"
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

    #[test]
    fn the_servers_database_spelled_differently_still_gets_the_servers_key_rules() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nExecStart=/usr/bin/temps serve --database-url={URL} --data-dir={}\n",
                data.display()
            ),
        );
        // `postgres://` and an explicit default port: the same database.
        let database = resolve(
            &unit,
            Some("postgres://temps:s3cr3t-pw@127.0.0.1:5432/temps"),
        );
        assert_eq!(service_data_dir(&database).unwrap(), Some(data.clone()));
        std::fs::create_dir_all(&data).unwrap();
        let error = encryption_key_in(&data, &database, Vec::new()).unwrap_err();
        assert!(
            matches!(error, ServerSecretsError::KeyNotCreated { .. }),
            "{error}"
        );
        assert!(
            !data.join(temps_config::ENCRYPTION_KEY_FILE).exists(),
            "a key was created"
        );
    }
}
