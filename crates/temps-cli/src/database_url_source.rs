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
        std::fs::write(dir.path().join(DATABASE_URL_CREDENTIAL), "postgres://cred").unwrap();
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
        std::fs::write(dir.path().join(DATABASE_URL_CREDENTIAL), "postgres://cred").unwrap();
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
        std::fs::write(&cred, "postgres://cred").unwrap();
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
        std::fs::write(&path, "postgres://u:p@db:5432/temps\n").unwrap();
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
        std::fs::write(&empty, "  \n").unwrap();
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
}
