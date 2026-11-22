// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep the database URL — password included — off the command line.
//!
//! Every command that talks to the control-plane database reads
//! `--database-url` / `TEMPS_DATABASE_URL` through clap. The flag form is
//! still accepted for compatibility, but it lands in `ps`, in
//! `/proc/<pid>/cmdline` until argv is scrubbed, and in `systemctl cat` of
//! any unit that carries it on `ExecStart=`. Two file-based sources avoid
//! that entirely:
//!
//! - `TEMPS_DATABASE_URL_FILE=/etc/temps/database-url` — a path to a file
//!   (mode `0600`) whose content is the URL.
//! - systemd credentials: `LoadCredential=temps_database_url:/etc/temps/database-url`
//!   makes systemd expose the file as `$CREDENTIALS_DIRECTORY/temps_database_url`,
//!   readable only by the service, without the unit itself containing the
//!   secret.
//!
//! [`adopt_database_url_file`] runs before clap parses (and before any
//! thread exists) and, when `TEMPS_DATABASE_URL` is not already set, exports
//! the file's content as `TEMPS_DATABASE_URL` for this process so every
//! command picks it up unchanged. These are bootstrap inputs — the URL is
//! needed before any database connection exists — so they are the
//! legitimate exception to "no runtime configuration in env vars".

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::service_definition::{self, ServiceDefinition, ServiceLocations, ServiceProbe};

/// The variable every database-backed command reads (via clap `env`).
pub const DATABASE_URL_ENV: &str = "TEMPS_DATABASE_URL";
/// Path to a file holding the database URL.
pub const DATABASE_URL_FILE_ENV: &str = "TEMPS_DATABASE_URL_FILE";
/// Name of the systemd credential (`LoadCredential=<name>:<path>`).
pub const DATABASE_URL_CREDENTIAL: &str = "temps_database_url";
/// The command-line flag that exposes the URL in process listings.
pub const DATABASE_URL_FLAG: &str = "--database-url";

/// Where `TEMPS_DATABASE_URL` came from, for a startup log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseUrlSource {
    /// Already in the environment (or not set at all): nothing adopted.
    Unchanged,
    /// Read from the file named by `TEMPS_DATABASE_URL_FILE`.
    File(PathBuf),
    /// Read from the systemd credential `temps_database_url`.
    SystemdCredential(PathBuf),
}

/// Why a configured database URL file could not be used. Configured but
/// unusable is fatal: silently falling through would start the command
/// without a database URL and fail later with a far less useful message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DatabaseUrlFileError {
    #[error("Cannot read the database URL from {source_name} '{path}': {reason}")]
    Unreadable {
        source_name: &'static str,
        path: String,
        reason: String,
    },
    #[error("The database URL file '{path}' ({source_name}) is empty")]
    Empty {
        source_name: &'static str,
        path: String,
    },
}

/// Resolve which file (if any) should supply the database URL, given the
/// relevant environment values. Pure, so the precedence is testable:
/// an explicit `TEMPS_DATABASE_URL` always wins, then
/// `TEMPS_DATABASE_URL_FILE`, then the systemd credential when present.
fn select_source(
    database_url: Option<&OsString>,
    url_file: Option<&OsString>,
    credentials_dir: Option<&OsString>,
) -> DatabaseUrlSource {
    if database_url.is_some_and(|v| !v.is_empty()) {
        return DatabaseUrlSource::Unchanged;
    }
    if let Some(path) = url_file.filter(|v| !v.is_empty()) {
        return DatabaseUrlSource::File(PathBuf::from(path));
    }
    if let Some(dir) = credentials_dir.filter(|v| !v.is_empty()) {
        let candidate = Path::new(dir).join(DATABASE_URL_CREDENTIAL);
        if candidate.is_file() {
            return DatabaseUrlSource::SystemdCredential(candidate);
        }
    }
    DatabaseUrlSource::Unchanged
}

/// Read a URL file: the whole content minus surrounding whitespace (a
/// trailing newline from `echo` or an editor must not end up in the URL).
fn read_url_file(path: &Path, source_name: &'static str) -> Result<String, DatabaseUrlFileError> {
    let raw = std::fs::read_to_string(path).map_err(|e| DatabaseUrlFileError::Unreadable {
        source_name,
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    let url = raw.trim();
    if url.is_empty() {
        return Err(DatabaseUrlFileError::Empty {
            source_name,
            path: path.display().to_string(),
        });
    }
    Ok(url.to_string())
}

/// What happened at startup, before clap parsed: whether the URL was passed
/// as a flag, and which file (if any) `TEMPS_DATABASE_URL` was adopted from.
/// Lets [`resolve_database_url`] say where the value clap produced came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupRecord {
    pub flag_on_command_line: bool,
    pub adopted: DatabaseUrlSource,
}

static STARTUP: OnceLock<StartupRecord> = OnceLock::new();

fn startup_record() -> StartupRecord {
    STARTUP.get().cloned().unwrap_or_else(|| StartupRecord {
        flag_on_command_line: database_url_on_command_line(std::env::args_os()),
        adopted: DatabaseUrlSource::Unchanged,
    })
}

/// Export the database URL from a file source as `TEMPS_DATABASE_URL`.
///
/// Mutates the process environment, so it must run before any thread is
/// spawned (it is called first thing in [`crate::run`]).
pub fn adopt_database_url_file() -> Result<DatabaseUrlSource, DatabaseUrlFileError> {
    let source = select_source(
        std::env::var_os(DATABASE_URL_ENV).as_ref(),
        std::env::var_os(DATABASE_URL_FILE_ENV).as_ref(),
        std::env::var_os("CREDENTIALS_DIRECTORY").as_ref(),
    );
    let _ = STARTUP.set(StartupRecord {
        flag_on_command_line: database_url_on_command_line(std::env::args_os()),
        adopted: source.clone(),
    });
    let url = match &source {
        DatabaseUrlSource::Unchanged => return Ok(source),
        DatabaseUrlSource::File(path) => read_url_file(path, DATABASE_URL_FILE_ENV)?,
        DatabaseUrlSource::SystemdCredential(path) => {
            read_url_file(path, "the systemd credential temps_database_url")?
        }
    };
    // Single-threaded at this point (see the function docs).
    std::env::set_var(DATABASE_URL_ENV, url);
    Ok(source)
}

/// Which sources a command may take its database URL from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlDiscovery {
    /// Only this process's own configuration: the flag, `TEMPS_DATABASE_URL`,
    /// `TEMPS_DATABASE_URL_FILE` and the systemd credential. For `serve`,
    /// `proxy` and `setup`, which *are* the service (or provision it): taking
    /// another service's URL would start a second server against it.
    ThisProcess,
    /// The above, then the installed `temps serve` service definition. For
    /// admin commands run from a shell on the server's host, so they reach
    /// the database the server uses without repeating `--database-url`.
    InstalledService,
}

/// Where a resolved database URL came from. Describes the source, never the
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatabaseUrlOrigin {
    Flag,
    Environment,
    UrlFile(PathBuf),
    SystemdCredential(PathBuf),
    InstalledService {
        definition: PathBuf,
        kind: service_definition::ServiceKind,
        location: service_definition::UrlLocation,
    },
}

impl DatabaseUrlOrigin {
    pub(crate) fn describe(&self) -> String {
        match self {
            DatabaseUrlOrigin::Flag => format!("the {DATABASE_URL_FLAG} flag"),
            DatabaseUrlOrigin::Environment => format!("{DATABASE_URL_ENV} in this environment"),
            DatabaseUrlOrigin::UrlFile(path) => {
                format!("{} ({DATABASE_URL_FILE_ENV})", path.display())
            }
            DatabaseUrlOrigin::SystemdCredential(path) => {
                format!("the systemd credential {}", path.display())
            }
            DatabaseUrlOrigin::InstalledService {
                definition,
                kind,
                location,
            } => format!(
                "the installed service {} ({})",
                definition.display(),
                location.describe(*kind)
            ),
        }
    }
}

/// A database URL and where it came from. The URL is never in `Debug`.
#[derive(Clone)]
pub(crate) struct ResolvedDatabaseUrl {
    url: String,
    origin: DatabaseUrlOrigin,
    service: Option<ServiceDefinition>,
}

impl std::fmt::Debug for ResolvedDatabaseUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedDatabaseUrl")
            .field("url", &"[REDACTED]")
            .field("origin", &self.origin)
            .field("service", &self.service)
            .finish()
    }
}

impl ResolvedDatabaseUrl {
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn into_url(self) -> String {
        self.url
    }

    pub(crate) fn origin(&self) -> &DatabaseUrlOrigin {
        &self.origin
    }

    /// The installed service definition, when one was read.
    pub(crate) fn service(&self) -> Option<&ServiceDefinition> {
        self.service.as_ref()
    }

    /// The installed service, when this URL names the database that server
    /// connects to (see [`same_database`]).
    pub(crate) fn serving_service(&self) -> Option<&ServiceDefinition> {
        self.service.as_ref().filter(|service| {
            service
                .url()
                .is_some_and(|url| same_database(url, self.url.as_str()))
        })
    }

    /// A one-line notice for stderr when the URL was not given to this
    /// command but read from the installed service, so it never connects to
    /// a database silently.
    pub(crate) fn discovery_notice(&self) -> Option<String> {
        matches!(self.origin, DatabaseUrlOrigin::InstalledService { .. })
            .then(|| format!("Using the database URL from {}.", self.origin.describe()))
    }

    /// Print [`Self::discovery_notice`] to stderr, which scripts parsing a
    /// command's stdout do not read.
    pub(crate) fn announce_discovery(&self) {
        if let Some(notice) = self.discovery_notice() {
            eprintln!("{notice}");
        }
    }
}

/// One source [`resolve_database_url`] checked, and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckedSource {
    pub source: String,
    pub outcome: String,
}

/// No source supplied a database URL. Lists every source checked, in order,
/// and how to supply one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", render_not_found(.checked, *.discovery))]
pub(crate) struct DatabaseUrlNotFound {
    pub checked: Vec<CheckedSource>,
    pub discovery: UrlDiscovery,
}

fn render_not_found(checked: &[CheckedSource], discovery: UrlDiscovery) -> String {
    let mut message = String::from("No database URL was found. Checked, in order:\n");
    for (index, checked) in checked.iter().enumerate() {
        message.push_str(&format!(
            "  {}. {}: {}\n",
            index + 1,
            checked.source,
            checked.outcome
        ));
    }
    message.push_str(&format!(
        "Fix: set {DATABASE_URL_ENV}=postgres://... in this environment, point \
         {DATABASE_URL_FILE_ENV} at a 0600 file holding the URL, or pass {DATABASE_URL_FLAG}."
    ));
    if discovery == UrlDiscovery::InstalledService {
        message.push_str(
            " On the server's host, run the command as a user that can read the installed \
             service definition and the files it references (usually with sudo).",
        );
    }
    message
}

/// Resolve the database URL for a command.
///
/// `value` is what clap produced from `--database-url` / `TEMPS_DATABASE_URL`
/// (which [`adopt_database_url_file`] may have filled from a file). The
/// precedence is the one `temps serve` applies, then, for
/// [`UrlDiscovery::InstalledService`], the installed service definition:
///
/// 1. `--database-url`
/// 2. `TEMPS_DATABASE_URL`
/// 3. `TEMPS_DATABASE_URL_FILE`
/// 4. the systemd credential `temps_database_url`
/// 5. the installed service definition (systemd unit and drop-ins, or the
///    launchd agent), read in that service's own order.
pub(crate) fn resolve_database_url(
    value: Option<&str>,
    discovery: UrlDiscovery,
) -> Result<ResolvedDatabaseUrl, DatabaseUrlNotFound> {
    resolve_database_url_in(value, discovery, &ServiceLocations::installed())
}

/// [`resolve_database_url`] with the service definition looked for at
/// `locations` instead of where the installer puts it.
pub(crate) fn resolve_database_url_in(
    value: Option<&str>,
    discovery: UrlDiscovery,
    locations: &ServiceLocations,
) -> Result<ResolvedDatabaseUrl, DatabaseUrlNotFound> {
    resolve_with(
        value,
        &startup_record(),
        std::env::var_os("CREDENTIALS_DIRECTORY").is_some(),
        discovery,
        locations,
    )
}

fn resolve_with(
    value: Option<&str>,
    startup: &StartupRecord,
    credentials_directory_set: bool,
    discovery: UrlDiscovery,
    locations: &ServiceLocations,
) -> Result<ResolvedDatabaseUrl, DatabaseUrlNotFound> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    let probes = match discovery {
        UrlDiscovery::ThisProcess => Vec::new(),
        UrlDiscovery::InstalledService => service_definition::probe(locations),
    };
    let service = probes.iter().find_map(|probe| match probe {
        ServiceProbe::Found(definition) => Some((**definition).clone()),
        ServiceProbe::Absent { .. } | ServiceProbe::Ignored { .. } => None,
    });

    if let Some(url) = value {
        let origin = if startup.flag_on_command_line {
            DatabaseUrlOrigin::Flag
        } else {
            match &startup.adopted {
                DatabaseUrlSource::Unchanged => DatabaseUrlOrigin::Environment,
                DatabaseUrlSource::File(path) => DatabaseUrlOrigin::UrlFile(path.clone()),
                DatabaseUrlSource::SystemdCredential(path) => {
                    DatabaseUrlOrigin::SystemdCredential(path.clone())
                }
            }
        };
        return Ok(ResolvedDatabaseUrl {
            url: url.to_string(),
            origin,
            service,
        });
    }

    let mut checked = vec![
        CheckedSource {
            source: DATABASE_URL_FLAG.to_string(),
            outcome: "not passed".to_string(),
        },
        CheckedSource {
            source: DATABASE_URL_ENV.to_string(),
            outcome: "not set in this environment".to_string(),
        },
        CheckedSource {
            source: DATABASE_URL_FILE_ENV.to_string(),
            outcome: "not set in this environment".to_string(),
        },
        CheckedSource {
            source: format!("the systemd credential {DATABASE_URL_CREDENTIAL}"),
            outcome: if credentials_directory_set {
                format!("$CREDENTIALS_DIRECTORY has no {DATABASE_URL_CREDENTIAL}")
            } else {
                "not running under systemd with LoadCredential= ($CREDENTIALS_DIRECTORY is unset)"
                    .to_string()
            },
        },
    ];

    for probe in probes {
        match probe {
            ServiceProbe::Absent { path } => checked.push(CheckedSource {
                source: format!("the installed service {}", path.display()),
                outcome: "not found".to_string(),
            }),
            ServiceProbe::Ignored { path, reason } => checked.push(CheckedSource {
                source: format!("the installed service {}", path.display()),
                outcome: reason,
            }),
            ServiceProbe::Found(definition) => {
                let definition = *definition;
                match &definition.database_url {
                    Ok(Some((url, location))) => {
                        return Ok(ResolvedDatabaseUrl {
                            url: url.clone(),
                            origin: DatabaseUrlOrigin::InstalledService {
                                definition: definition.path.clone(),
                                kind: definition.kind,
                                location: location.clone(),
                            },
                            service: Some(definition),
                        });
                    }
                    Ok(None) => {
                        let mut outcome =
                            "found, but it sets no database URL for `temps serve`".to_string();
                        for problem in &definition.problems {
                            outcome.push_str("; ");
                            outcome.push_str(problem);
                        }
                        checked.push(CheckedSource {
                            source: format!("the installed service {}", definition.path.display()),
                            outcome,
                        });
                    }
                    Err(reason) => checked.push(CheckedSource {
                        source: format!("the installed service {}", definition.path.display()),
                        outcome: format!("found, but its database URL cannot be read: {reason}"),
                    }),
                }
            }
        }
    }

    Err(DatabaseUrlNotFound { checked, discovery })
}

/// Whether two database URLs name the same database: identical, or
/// PostgreSQL URLs with the same host, port and database once spelling is
/// set aside (`postgres://` or `postgresql://`, host case, the default port
/// 5432, a socket `host=` / `port=` / `dbname=` parameter, credentials and
/// other parameters such as `sslmode`). A host given two ways (`localhost`
/// and `127.0.0.1`) still counts as different: whether they are the same
/// server cannot be told from the URL.
pub(crate) fn same_database(a: &str, b: &str) -> bool {
    a == b
        || matches!(
            (database_identity(a), database_identity(b)),
            (Some(a), Some(b)) if a == b
        )
}

/// `(host, port, database)` of a PostgreSQL URL.
fn database_identity(raw: &str) -> Option<(String, u16, String)> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return None;
    }
    let parameter = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
    };
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .or_else(|| parameter("host"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let port = match url.port() {
        Some(port) => port,
        None => match parameter("port") {
            Some(port) => port.parse().ok()?,
            None => 5432,
        },
    };
    let path = url.path().trim_start_matches('/');
    let database = if path.is_empty() {
        // libpq's default: the database named after the user.
        parameter("dbname").unwrap_or_else(|| url.username().to_string())
    } else {
        urlencoding::decode(path).ok()?.into_owned()
    };
    Some((host, port, database))
}

/// Whether the raw command line passes the database URL as a flag
/// (`--database-url <url>` or `--database-url=<url>`). Checked before argv
/// is scrubbed so `serve`/`proxy` can tell the operator to move it.
pub fn database_url_on_command_line<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let prefix = format!("{DATABASE_URL_FLAG}=");
    args.into_iter().skip(1).any(|arg| {
        let arg = arg.as_ref().to_string_lossy();
        arg == DATABASE_URL_FLAG || arg.starts_with(prefix.as_str())
    })
}

/// The warning printed when [`database_url_on_command_line`] is true.
pub fn command_line_warning() -> String {
    format!(
        "The database URL was passed with {DATABASE_URL_FLAG}, so its password is visible \
         in the service definition (e.g. `systemctl cat temps`) and briefly in process \
         listings. Move it out of the command line: put \
         `{DATABASE_URL_ENV}=postgres://...` in a root-owned 0600 file such as \
         /etc/temps/temps.env and reference it with `EnvironmentFile=/etc/temps/temps.env`, \
         or store the bare URL in a 0600 file and use \
         `LoadCredential={DATABASE_URL_CREDENTIAL}:/etc/temps/database-url` \
         (or {DATABASE_URL_FILE_ENV}=/etc/temps/database-url). Then drop \
         {DATABASE_URL_FLAG} from ExecStart."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn explicit_env_url_wins_over_every_file_source() {
        let dir = tempfile::tempdir().unwrap();
        crate::service_definition::test_support::write_trusted(
            dir.path().join(DATABASE_URL_CREDENTIAL),
            "postgres://cred",
        )
        .unwrap();
        let source = select_source(
            Some(&os("postgres://env")),
            Some(&os("/etc/temps/database-url")),
            Some(&dir.path().as_os_str().to_owned()),
        );
        assert_eq!(source, DatabaseUrlSource::Unchanged);
    }

    #[test]
    fn url_file_wins_over_the_systemd_credential() {
        let dir = tempfile::tempdir().unwrap();
        crate::service_definition::test_support::write_trusted(
            dir.path().join(DATABASE_URL_CREDENTIAL),
            "postgres://cred",
        )
        .unwrap();
        let source = select_source(
            None,
            Some(&os("/etc/temps/database-url")),
            Some(&dir.path().as_os_str().to_owned()),
        );
        assert_eq!(
            source,
            DatabaseUrlSource::File(PathBuf::from("/etc/temps/database-url"))
        );
    }

    #[test]
    fn systemd_credential_is_used_only_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let credentials_dir = dir.path().as_os_str().to_owned();
        assert_eq!(
            select_source(None, None, Some(&credentials_dir)),
            DatabaseUrlSource::Unchanged,
            "a credentials directory without our credential must be ignored"
        );
        let cred = dir.path().join(DATABASE_URL_CREDENTIAL);
        crate::service_definition::test_support::write_trusted(&cred, "postgres://cred").unwrap();
        assert_eq!(
            select_source(None, None, Some(&credentials_dir)),
            DatabaseUrlSource::SystemdCredential(cred)
        );
    }

    #[test]
    fn empty_values_count_as_unset() {
        assert_eq!(
            select_source(Some(&os("")), Some(&os("")), Some(&os(""))),
            DatabaseUrlSource::Unchanged
        );
    }

    #[test]
    fn url_file_content_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database-url");
        crate::service_definition::test_support::write_trusted(
            &path,
            "postgres://u:p@db:5432/temps\n",
        )
        .unwrap();
        assert_eq!(
            read_url_file(&path, DATABASE_URL_FILE_ENV).unwrap(),
            "postgres://u:p@db:5432/temps"
        );
    }

    #[test]
    fn missing_or_empty_url_file_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let err = read_url_file(&missing, DATABASE_URL_FILE_ENV).unwrap_err();
        assert!(matches!(err, DatabaseUrlFileError::Unreadable { .. }));
        assert!(err.to_string().contains(&missing.display().to_string()));

        let empty = dir.path().join("empty");
        crate::service_definition::test_support::write_trusted(&empty, "  \n").unwrap();
        let err = read_url_file(&empty, DATABASE_URL_FILE_ENV).unwrap_err();
        assert!(matches!(err, DatabaseUrlFileError::Empty { .. }));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn detects_both_flag_forms_but_not_lookalikes() {
        assert!(database_url_on_command_line([
            "temps",
            "serve",
            "--database-url",
            "postgres://x"
        ]));
        assert!(database_url_on_command_line([
            "temps",
            "serve",
            "--database-url=postgres://x"
        ]));
        assert!(!database_url_on_command_line(["temps", "serve"]));
        assert!(!database_url_on_command_line([
            "temps",
            "serve",
            "--database-url-file=/etc/temps/database-url"
        ]));
        // argv[0] is never treated as a flag.
        assert!(!database_url_on_command_line(["--database-url"]));
    }

    #[test]
    fn warning_never_contains_a_url_value_and_names_every_alternative() {
        let warning = command_line_warning();
        assert!(warning.contains("EnvironmentFile="));
        assert!(warning.contains("LoadCredential=temps_database_url:"));
        assert!(warning.contains(DATABASE_URL_FILE_ENV));
        assert!(!warning.contains('@'), "no credentials in the warning");
    }

    // ── resolve_database_url ────────────────────────────────────────

    const PASSWORD: &str = "pw-8f3a91c2";

    fn url(host: &str) -> String {
        format!("postgresql://temps:{PASSWORD}@{host}:5432/temps?sslmode=disable")
    }

    fn startup(flag: bool, adopted: DatabaseUrlSource) -> StartupRecord {
        StartupRecord {
            flag_on_command_line: flag,
            adopted,
        }
    }

    /// The installer's unit: the URL and data directory as `serve` flags.
    fn installer_unit(dir: &Path, database_url: &str) -> ServiceLocations {
        let unit = dir.join("temps.service");
        crate::service_definition::test_support::write_trusted(
            &unit,
            format!(
                "[Service]\nWorkingDirectory={home}/.temps\n\
                 ExecStart={home}/.temps/bin/temps serve \\\n  --database-url=\"{database_url}\" \\\n  \
                 --data-dir=\"{home}/.temps/data\"\nEnvironment=HOME={home}\n",
                home = dir.display()
            ),
        )
        .unwrap();
        ServiceLocations {
            systemd_unit: Some(unit),
            launchd_agent: None,
        }
    }

    fn resolve(
        value: Option<&str>,
        startup: &StartupRecord,
        discovery: UrlDiscovery,
        locations: &ServiceLocations,
    ) -> Result<ResolvedDatabaseUrl, DatabaseUrlNotFound> {
        resolve_with(value, startup, false, discovery, locations)
    }

    #[test]
    fn the_flag_wins_and_is_named_as_the_origin() {
        let dir = tempfile::tempdir().unwrap();
        let locations = installer_unit(dir.path(), &url("service-db"));
        let resolved = resolve(
            Some(&url("flag-db")),
            &startup(true, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &locations,
        )
        .unwrap();
        assert_eq!(resolved.url(), url("flag-db"));
        assert_eq!(resolved.origin(), &DatabaseUrlOrigin::Flag);
        assert_eq!(resolved.origin().describe(), "the --database-url flag");
        assert_eq!(resolved.discovery_notice(), None);
    }

    #[test]
    fn the_environment_and_adopted_files_are_named_as_the_origin() {
        let none = ServiceLocations::default();
        let env = resolve(
            Some(&url("env-db")),
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &none,
        )
        .unwrap();
        assert_eq!(env.origin(), &DatabaseUrlOrigin::Environment);
        assert_eq!(
            env.origin().describe(),
            "TEMPS_DATABASE_URL in this environment"
        );

        let file = PathBuf::from("/etc/temps/database-url");
        let adopted = resolve(
            Some(&url("file-db")),
            &startup(false, DatabaseUrlSource::File(file.clone())),
            UrlDiscovery::ThisProcess,
            &none,
        )
        .unwrap();
        assert_eq!(adopted.origin(), &DatabaseUrlOrigin::UrlFile(file));

        let credential = PathBuf::from("/run/credentials/temps.service/temps_database_url");
        let adopted = resolve(
            Some(&url("cred-db")),
            &startup(
                false,
                DatabaseUrlSource::SystemdCredential(credential.clone()),
            ),
            UrlDiscovery::ThisProcess,
            &none,
        )
        .unwrap();
        assert_eq!(
            adopted.origin(),
            &DatabaseUrlOrigin::SystemdCredential(credential)
        );
    }

    #[test]
    fn without_one_the_installers_unit_supplies_the_url_and_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let locations = installer_unit(dir.path(), &url("127.0.0.1"));
        let resolved = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &locations,
        )
        .unwrap();
        assert_eq!(resolved.url(), url("127.0.0.1"));
        assert_eq!(
            resolved.origin(),
            &DatabaseUrlOrigin::InstalledService {
                definition: dir.path().join("temps.service"),
                kind: service_definition::ServiceKind::Systemd,
                location: service_definition::UrlLocation::CommandLine,
            }
        );
        assert_eq!(
            crate::server_secrets::service_data_dir(&resolved).unwrap(),
            Some(dir.path().join(".temps/data"))
        );
        let notice = resolved.discovery_notice().unwrap();
        assert!(notice.contains("temps.service"), "{notice}");
        assert!(notice.contains("--database-url on ExecStart="), "{notice}");
    }

    #[test]
    fn the_installers_environment_file_supplies_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("temps.env");
        crate::service_definition::test_support::write_trusted(
            &env_file,
            format!("TEMPS_DATABASE_URL={}\n", url("env-file-db")),
        )
        .unwrap();
        let unit = dir.path().join("temps.service");
        crate::service_definition::test_support::write_trusted(
            &unit,
            format!(
                "[Service]\nEnvironmentFile={}\nExecStart=/usr/local/bin/temps serve\n",
                env_file.display()
            ),
        )
        .unwrap();
        let resolved = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &ServiceLocations {
                systemd_unit: Some(unit),
                launchd_agent: None,
            },
        )
        .unwrap();
        assert_eq!(resolved.url(), url("env-file-db"));
        assert!(
            resolved.origin().describe().contains(&format!(
                "TEMPS_DATABASE_URL in EnvironmentFile={}",
                env_file.display()
            )),
            "{}",
            resolved.origin().describe()
        );
    }

    #[test]
    fn nothing_found_lists_every_source_in_order_and_how_to_fix_it() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("temps.service");
        let error = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &ServiceLocations {
                systemd_unit: Some(unit.clone()),
                launchd_agent: None,
            },
        )
        .unwrap_err();
        let message = error.to_string();
        let expected = [
            "1. --database-url: not passed",
            "2. TEMPS_DATABASE_URL: not set",
            "3. TEMPS_DATABASE_URL_FILE: not set",
            "4. the systemd credential temps_database_url:",
            &format!("5. the installed service {}: not found", unit.display()),
            "Fix: set TEMPS_DATABASE_URL=",
            "sudo",
        ];
        let mut from = 0;
        for part in expected {
            let at = message[from..]
                .find(part)
                .unwrap_or_else(|| panic!("{part:?} missing or out of order in:\n{message}"));
            from += at + part.len();
        }

        // The server itself never consults the installed service.
        let message = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::ThisProcess,
            &installer_unit(dir.path(), &url("service-db")),
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("4. the systemd credential"), "{message}");
        assert!(!message.contains("installed service"), "{message}");
    }

    #[test]
    fn an_unreadable_environment_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("temps.service");
        crate::service_definition::test_support::write_trusted(
            &unit,
            "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE=/nonexistent/temps/database-url\n\
             ExecStart=/usr/local/bin/temps serve\n",
        )
        .unwrap();
        let message = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &ServiceLocations {
                systemd_unit: Some(unit),
                launchd_agent: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            message.contains("its database URL cannot be read")
                && message.contains("/nonexistent/temps/database-url"),
            "{message}"
        );
    }

    #[test]
    fn this_process_mode_ignores_an_installed_service() {
        let dir = tempfile::tempdir().unwrap();
        let locations = installer_unit(dir.path(), &url("service-db"));
        let resolved = resolve(
            Some(&url("env-db")),
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::ThisProcess,
            &locations,
        )
        .unwrap();
        assert!(resolved.service().is_none());
        assert_eq!(
            crate::server_secrets::service_data_dir(&resolved).unwrap(),
            None
        );
    }

    #[test]
    fn the_service_data_dir_is_used_only_with_the_services_own_database() {
        let dir = tempfile::tempdir().unwrap();
        let locations = installer_unit(dir.path(), &url("service-db"));
        let same = resolve(
            Some(&url("service-db")),
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &locations,
        )
        .unwrap();
        assert_eq!(
            crate::server_secrets::service_data_dir(&same).unwrap(),
            Some(dir.path().join(".temps/data"))
        );
        let other = resolve(
            Some(&url("staging-db")),
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &locations,
        )
        .unwrap();
        assert_eq!(
            crate::server_secrets::service_data_dir(&other).unwrap(),
            None
        );
    }

    #[test]
    fn an_empty_value_counts_as_missing() {
        let error = resolve(
            Some("   "),
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::ThisProcess,
            &ServiceLocations::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("No database URL was found"));
    }

    #[test]
    fn no_output_of_the_resolver_contains_the_url_or_its_password() {
        let dir = tempfile::tempdir().unwrap();
        let locations = installer_unit(dir.path(), &url("127.0.0.1"));
        let resolved = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &locations,
        )
        .unwrap();
        let outputs = [
            format!("{resolved:?}"),
            resolved.origin().describe(),
            resolved.discovery_notice().unwrap_or_default(),
            format!("{:?}", resolved.service()),
        ];
        for output in outputs {
            assert!(!output.contains(PASSWORD), "{output}");
            assert!(!output.contains("postgresql://"), "{output}");
        }

        // A unit that carries a URL but does not run `serve`: the reason is
        // reported, the URL is not.
        let unit = dir.path().join("proxy.service");
        crate::service_definition::test_support::write_trusted(
            &unit,
            format!(
                "[Service]\nExecStart=/usr/local/bin/temps proxy --database-url={}\n",
                url("127.0.0.1")
            ),
        )
        .unwrap();
        let message = resolve(
            None,
            &startup(false, DatabaseUrlSource::Unchanged),
            UrlDiscovery::InstalledService,
            &ServiceLocations {
                systemd_unit: Some(unit),
                launchd_agent: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("does not run `temps serve`"), "{message}");
        assert!(!message.contains(PASSWORD), "{message}");
    }

    #[test]
    fn the_same_database_spelled_differently_is_recognised() {
        let base = "postgresql://temps:pw@127.0.0.1:5432/temps?sslmode=disable";
        for same in [
            "postgres://temps:pw@127.0.0.1/temps",
            "postgresql://other-user:other-pw@127.0.0.1:5432/temps?sslmode=require",
            "POSTGRES://temps@127.0.0.1:5432/temps",
        ] {
            assert!(same_database(base, same), "{same}");
        }
        assert!(same_database(
            "postgresql:///temps?host=/var/run/postgresql",
            "postgres:///temps?host=/var/run/postgresql&port=5432"
        ));
        for different in [
            "postgresql://temps:pw@127.0.0.1:5433/temps",
            "postgresql://temps:pw@127.0.0.1:5432/other",
            "postgresql://temps:pw@10.0.0.2:5432/temps",
            // Possibly the same server, but that cannot be told from the URL.
            "postgresql://temps:pw@localhost:5432/temps",
            "not a url",
        ] {
            assert!(!same_database(base, different), "{different}");
        }
    }
}
