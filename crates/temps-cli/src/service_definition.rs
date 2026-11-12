// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The installed `temps serve` service definition, read the way its
//! supervisor reads it.
//!
//! The installer runs Temps under systemd (`/etc/systemd/system/temps.service`)
//! on Linux and as a launchd agent (`~/Library/LaunchAgents/dev.temps.temps.plist`)
//! on macOS, with the database URL and data directory as `serve` flags. A
//! command run from a shell (`temps doctor`, `temps migrate`) inherits none of
//! that, so without this module it either asks for `--database-url` or looks
//! at a different data directory from the server's. This module answers "what
//! does the installed server use?" from the definition itself: the
//! `ExecStart=` / `ProgramArguments` flags, `Environment=`,
//! `EnvironmentFile=`, `LoadCredential=` and drop-ins, in the order the
//! server applies them.
//!
//! Only files that cannot be rewritten by another user are trusted, since
//! the answer decides which database an admin command (possibly run as root)
//! connects to. Nothing read here is ever printed: callers describe *where* a
//! value came from, never the value.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::database_url_source::{
    DATABASE_URL_CREDENTIAL, DATABASE_URL_ENV, DATABASE_URL_FILE_ENV,
};

/// Where the installer writes the systemd unit.
pub(crate) const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/temps.service";
/// The launchd agent the installer writes, relative to the home directory.
pub(crate) const LAUNCHD_AGENT_PATH: &str = "Library/LaunchAgents/dev.temps.temps.plist";
/// The variable `serve` reads its data directory from.
pub(crate) const DATA_DIR_ENV: &str = "TEMPS_DATA_DIR";

/// Larger than any service definition or URL file; a bigger file is not one.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Which supervisor the definition is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceKind {
    Systemd,
    Launchd,
}

/// Where in the service definition the database URL was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlLocation {
    /// `--database-url` on `ExecStart=` / in `ProgramArguments`.
    CommandLine,
    /// `TEMPS_DATABASE_URL` in `Environment=` / `EnvironmentVariables`.
    Environment,
    /// `TEMPS_DATABASE_URL` in this `EnvironmentFile=`.
    EnvironmentFile(PathBuf),
    /// The file `TEMPS_DATABASE_URL_FILE` names.
    UrlFile(PathBuf),
    /// `LoadCredential=temps_database_url:<path>`.
    Credential(PathBuf),
}

impl UrlLocation {
    pub(crate) fn describe(&self, kind: ServiceKind) -> String {
        match (self, kind) {
            (UrlLocation::CommandLine, ServiceKind::Systemd) => {
                "--database-url on ExecStart=".to_string()
            }
            (UrlLocation::CommandLine, ServiceKind::Launchd) => {
                "--database-url in ProgramArguments".to_string()
            }
            (UrlLocation::Environment, ServiceKind::Systemd) => {
                format!("{DATABASE_URL_ENV} in Environment=")
            }
            (UrlLocation::Environment, ServiceKind::Launchd) => {
                format!("{DATABASE_URL_ENV} in EnvironmentVariables")
            }
            (UrlLocation::EnvironmentFile(path), _) => {
                format!("{DATABASE_URL_ENV} in EnvironmentFile={}", path.display())
            }
            (UrlLocation::UrlFile(path), _) => {
                format!("{DATABASE_URL_FILE_ENV}={}", path.display())
            }
            (UrlLocation::Credential(path), _) => {
                format!(
                    "LoadCredential={DATABASE_URL_CREDENTIAL}:{}",
                    path.display()
                )
            }
        }
    }
}

/// Where in the service definition the data directory was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataDirLocation {
    /// `--data-dir` on the command line.
    CommandLine,
    /// `TEMPS_DATA_DIR` in the service environment.
    Environment,
    /// `serve`'s default, `$HOME/.temps`, with the service's `HOME`.
    HomeDefault,
}

impl DataDirLocation {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            DataDirLocation::CommandLine => "--data-dir",
            DataDirLocation::Environment => "TEMPS_DATA_DIR",
            DataDirLocation::HomeDefault => "the service's $HOME/.temps",
        }
    }
}

/// What the installed `temps serve` runs with.
#[derive(Clone)]
pub(crate) struct ServiceDefinition {
    /// The unit or plist.
    pub path: PathBuf,
    pub kind: ServiceKind,
    /// The database URL and where it was found; `Err` says why it could not
    /// be read from where the server reads it (for example a root-only
    /// environment file read by an unprivileged user).
    pub database_url: Result<Option<(String, UrlLocation)>, String>,
    pub data_dir: Option<(PathBuf, DataDirLocation)>,
    /// The server's working directory, where it also looks for GeoLite2.
    pub working_directory: Option<PathBuf>,
    /// The effective service environment (`Environment=` then
    /// `EnvironmentFile=`, which overrides it).
    pub environment: BTreeMap<String, String>,
    /// Files the definition references that could not be read.
    pub problems: Vec<String>,
}

impl std::fmt::Debug for ServiceDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceDefinition")
            .field("path", &self.path)
            .field("kind", &self.kind)
            .field(
                "database_url",
                &self
                    .database_url
                    .as_ref()
                    .map(|found| found.as_ref().map(|(_, location)| location)),
            )
            .field("data_dir", &self.data_dir)
            .field("working_directory", &self.working_directory)
            .field("environment", &self.environment.keys().collect::<Vec<_>>())
            .field("problems", &self.problems)
            .finish()
    }
}

impl ServiceDefinition {
    /// The URL the server connects with, when it could be read.
    pub(crate) fn url(&self) -> Option<&str> {
        match &self.database_url {
            Ok(Some((url, _))) => Some(url),
            Ok(None) | Err(_) => None,
        }
    }

    pub(crate) fn env(&self, name: &str) -> Option<&str> {
        self.environment
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }
}

/// The outcome of looking at one candidate location.
#[derive(Debug, Clone)]
pub(crate) enum ServiceProbe {
    Absent {
        path: PathBuf,
    },
    /// Present but not used, with the reason.
    Ignored {
        path: PathBuf,
        reason: String,
    },
    Found(Box<ServiceDefinition>),
}

/// The locations a definition may be installed at, in the order checked.
#[derive(Debug, Clone, Default)]
pub(crate) struct ServiceLocations {
    pub systemd_unit: Option<PathBuf>,
    pub launchd_agent: Option<PathBuf>,
}

impl ServiceLocations {
    /// Where the installer puts the definition on this platform.
    pub(crate) fn installed() -> Self {
        let mut locations = ServiceLocations::default();
        if cfg!(target_os = "linux") {
            locations.systemd_unit = Some(PathBuf::from(SYSTEMD_UNIT_PATH));
        }
        if cfg!(target_os = "macos") {
            locations.launchd_agent = dirs::home_dir().map(|home| home.join(LAUNCHD_AGENT_PATH));
        }
        locations
    }

    /// Every candidate path, for the "nothing found" message.
    pub(crate) fn paths(&self) -> Vec<&Path> {
        [self.systemd_unit.as_deref(), self.launchd_agent.as_deref()]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// Look at every candidate location.
pub(crate) fn probe(locations: &ServiceLocations) -> Vec<ServiceProbe> {
    let mut probes = Vec::new();
    if let Some(unit) = &locations.systemd_unit {
        probes.push(probe_file(unit, |contents| {
            let mut sources = vec![(unit.clone(), contents)];
            sources.extend(read_drop_ins(unit)?);
            Ok(systemd_definition(unit, &sources))
        }));
    }
    if let Some(agent) = &locations.launchd_agent {
        probes.push(probe_file(agent, |contents| {
            let home = agent
                .ancestors()
                .nth(3)
                .map(Path::to_path_buf)
                .filter(|home| home.join(LAUNCHD_AGENT_PATH) == *agent);
            Ok(launchd_definition(agent, &contents, home.as_deref()))
        }));
    }
    probes
}

/// The first definition found, if any.
pub(crate) fn installed() -> Option<ServiceDefinition> {
    probe(&ServiceLocations::installed())
        .into_iter()
        .find_map(|probe| match probe {
            ServiceProbe::Found(definition) => Some(*definition),
            ServiceProbe::Absent { .. } | ServiceProbe::Ignored { .. } => None,
        })
}

fn probe_file(
    path: &Path,
    parse: impl FnOnce(String) -> Result<ServiceDefinition, String>,
) -> ServiceProbe {
    match read_trusted(path) {
        Ok(None) => ServiceProbe::Absent {
            path: path.to_path_buf(),
        },
        Ok(Some(contents)) => match parse(contents) {
            Ok(definition) => ServiceProbe::Found(Box::new(definition)),
            Err(reason) => ServiceProbe::Ignored {
                path: path.to_path_buf(),
                reason,
            },
        },
        Err(reason) => ServiceProbe::Ignored {
            path: path.to_path_buf(),
            reason,
        },
    }
}

/// Read a file the service definition depends on: `Ok(None)` when it does
/// not exist, an error naming the reason when it exists but cannot be used.
fn read_trusted(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(format!(
                "cannot be read by this user ({error}); run the command with sudo or as the \
                 service's user"
            ))
        }
        Err(error) => return Err(format!("cannot be read: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot be inspected: {error}"))?;
    if !metadata.is_file() {
        return Err("is not a regular file".to_string());
    }
    check_not_writable_by_others(&metadata)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "is {} bytes, larger than the {MAX_FILE_BYTES}-byte limit for a service file",
            metadata.len()
        ));
    }
    let mut contents = String::new();
    file.take(MAX_FILE_BYTES)
        .read_to_string(&mut contents)
        .map_err(|error| format!("cannot be read: {error}"))?;
    Ok(Some(contents))
}

/// A file another user can rewrite could point a root-run admin command at a
/// database (and data directory) of that user's choosing, so it is not used.
#[cfg(unix)]
fn check_not_writable_by_others(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid/getegid cannot fail and touch no memory.
    let (euid, egid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let mode = metadata.mode();
    if metadata.uid() != 0 && metadata.uid() != euid {
        return Err(format!(
            "is owned by uid {}, neither root nor this user, so it is not trusted",
            metadata.uid()
        ));
    }
    if mode & 0o002 != 0 {
        return Err("is writable by every user, so it is not trusted".to_string());
    }
    if mode & 0o020 != 0 && metadata.gid() != 0 && metadata.gid() != egid {
        return Err(format!(
            "is writable by group {}, so it is not trusted",
            metadata.gid()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_not_writable_by_others(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// `<unit>.d/*.conf`, in the lexical order systemd applies them.
fn read_drop_ins(unit: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut dir = unit.as_os_str().to_owned();
    dir.push(".d");
    let dir = PathBuf::from(dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "its drop-in directory {} cannot be read: {error}",
                dir.display()
            ))
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "conf"))
        .collect();
    paths.sort();
    let mut drop_ins = Vec::new();
    for path in paths {
        match read_trusted(&path) {
            Ok(Some(contents)) => drop_ins.push((path, contents)),
            Ok(None) => {}
            Err(reason) => {
                return Err(format!("its drop-in {} {reason}", path.display()));
            }
        }
    }
    Ok(drop_ins)
}

/// The `[Service]` settings that decide what `serve` runs with.
#[derive(Debug, Default)]
struct SystemdService {
    exec_start: Option<String>,
    environment: Vec<String>,
    environment_files: Vec<String>,
    load_credentials: Vec<String>,
    working_directory: Option<String>,
}

/// Apply the unit and its drop-ins in order, as systemd does: a later
/// assignment overrides a single-valued setting, and an empty one resets a
/// list.
fn parse_systemd_service(sources: &[(PathBuf, String)]) -> SystemdService {
    let mut service = SystemdService::default();
    for (_, contents) in sources {
        let mut in_service = false;
        for line in logical_lines(contents) {
            let line = line.trim();
            if line.starts_with('[') {
                in_service = line == "[Service]";
                continue;
            }
            if !in_service {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            let list = match key.trim() {
                "ExecStart" => {
                    service.exec_start = (!value.is_empty()).then_some(value);
                    continue;
                }
                "WorkingDirectory" => {
                    service.working_directory = (!value.is_empty()).then_some(value);
                    continue;
                }
                "Environment" => &mut service.environment,
                "EnvironmentFile" => &mut service.environment_files,
                "LoadCredential" => &mut service.load_credentials,
                _ => continue,
            };
            if value.is_empty() {
                list.clear();
            } else {
                list.push(value);
            }
        }
    }
    service
}

/// Lines with `\` continuations joined (the backslash becomes a space) and
/// comment lines dropped.
fn logical_lines(contents: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current: Option<String> = None;
    for raw in contents.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let (text, continues) = match raw.trim_end().strip_suffix('\\') {
            Some(text) => (text, true),
            None => (raw, false),
        };
        let line = current.get_or_insert_with(String::new);
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(text);
        if !continues {
            lines.extend(current.take());
        }
    }
    lines.extend(current);
    lines
}

fn systemd_definition(unit: &Path, sources: &[(PathBuf, String)]) -> ServiceDefinition {
    let service = parse_systemd_service(sources);
    let mut problems = Vec::new();

    let mut environment = BTreeMap::new();
    for assignment in service
        .environment
        .iter()
        .flat_map(|value| split_words(&resolve_specifiers(value)))
    {
        if let Some((key, value)) = assignment.split_once('=') {
            environment.insert(key.to_string(), value.to_string());
        }
    }
    // EnvironmentFile= overrides Environment=, later files override earlier.
    for entry in &service.environment_files {
        let entry = resolve_specifiers(entry);
        let (optional, path) = match entry.strip_prefix('-') {
            Some(path) => (true, path),
            None => (false, entry.as_str()),
        };
        let path = PathBuf::from(path.trim());
        match read_trusted(&path) {
            Ok(Some(contents)) => {
                for (key, value) in parse_environment_file(&contents) {
                    environment.insert(key, value);
                }
            }
            Ok(None) if optional => {}
            Ok(None) => problems.push(format!(
                "EnvironmentFile={} does not exist (systemd refuses to start the unit)",
                path.display()
            )),
            Err(reason) => problems.push(format!("EnvironmentFile={} {reason}", path.display())),
        }
    }

    let working_directory = service
        .working_directory
        .as_deref()
        .map(resolve_specifiers)
        .and_then(|dir| {
            let dir = dir.strip_prefix('-').unwrap_or(&dir).to_string();
            expand_home(&dir, environment.get("HOME").map(String::as_str))
        });

    let arguments = service.exec_start.as_deref().map(|exec_start| {
        let exec_start = exec_start.trim_start_matches(['@', '-', ':', '+', '!']);
        split_words(&resolve_specifiers(exec_start))
            .into_iter()
            .map(|word| expand_variables(&word, &environment))
            .collect::<Vec<_>>()
    });
    let serve_arguments = arguments.as_deref().filter(|args| runs_serve(args));
    if arguments.is_some() && serve_arguments.is_none() {
        problems.push("ExecStart= does not run `temps serve`".to_string());
    }

    let credential = service
        .load_credentials
        .iter()
        .map(|entry| resolve_specifiers(entry))
        .find_map(|entry| {
            let (id, path) = entry.split_once(':')?;
            (id.trim() == DATABASE_URL_CREDENTIAL).then(|| PathBuf::from(path.trim()))
        });

    let database_url = serve_database_url(
        serve_arguments,
        &environment,
        &service.environment_files,
        credential,
    );
    let data_dir = serve_data_dir(
        serve_arguments,
        &environment,
        environment.get("HOME").map(PathBuf::from),
        working_directory.as_deref(),
    );

    ServiceDefinition {
        path: unit.to_path_buf(),
        kind: ServiceKind::Systemd,
        database_url,
        data_dir,
        working_directory,
        environment,
        problems,
    }
}

/// `serve` with `--database-url` wins over `TEMPS_DATABASE_URL`, which wins
/// over `TEMPS_DATABASE_URL_FILE`, which wins over the credential: the same
/// order as `temps serve` itself (clap, then `adopt_database_url_file`).
fn serve_database_url(
    arguments: Option<&[String]>,
    environment: &BTreeMap<String, String>,
    environment_files: &[String],
    credential: Option<PathBuf>,
) -> Result<Option<(String, UrlLocation)>, String> {
    if let Some(url) = arguments.and_then(|args| flag_value(args, "--database-url")) {
        return Ok(Some((url, UrlLocation::CommandLine)));
    }
    if let Some(url) = environment
        .get(DATABASE_URL_ENV)
        .filter(|url| !url.trim().is_empty())
    {
        let location = environment_file_defining(environment_files, DATABASE_URL_ENV)
            .map(UrlLocation::EnvironmentFile)
            .unwrap_or(UrlLocation::Environment);
        return Ok(Some((url.trim().to_string(), location)));
    }
    if let Some(path) = environment
        .get(DATABASE_URL_FILE_ENV)
        .filter(|path| !path.trim().is_empty())
    {
        let path = PathBuf::from(path.trim());
        return read_url(&path).map(|url| Some((url, UrlLocation::UrlFile(path))));
    }
    if let Some(path) = credential {
        if !path.is_absolute() {
            return Err(format!(
                "LoadCredential={DATABASE_URL_CREDENTIAL}:{} is not an absolute path",
                path.display()
            ));
        }
        return read_url(&path).map(|url| Some((url, UrlLocation::Credential(path))));
    }
    Ok(None)
}

fn read_url(path: &Path) -> Result<String, String> {
    match read_trusted(path) {
        Ok(Some(contents)) if !contents.trim().is_empty() => Ok(contents.trim().to_string()),
        Ok(Some(_)) => Err(format!("{} is empty", path.display())),
        Ok(None) => Err(format!("{} does not exist", path.display())),
        Err(reason) => Err(format!("{} {reason}", path.display())),
    }
}

/// The last environment file that sets `name` (later files win), so the
/// origin names the file the value really came from.
fn environment_file_defining(environment_files: &[String], name: &str) -> Option<PathBuf> {
    environment_files.iter().rev().find_map(|entry| {
        let entry = resolve_specifiers(entry);
        let path = PathBuf::from(entry.strip_prefix('-').unwrap_or(&entry).trim());
        let contents = read_trusted(&path).ok().flatten()?;
        parse_environment_file(&contents)
            .into_iter()
            .any(|(key, _)| key == name)
            .then_some(path)
    })
}

/// `serve`'s data directory: `--data-dir`, `TEMPS_DATA_DIR`, then
/// `$HOME/.temps`. A relative path is relative to the working directory.
fn serve_data_dir(
    arguments: Option<&[String]>,
    environment: &BTreeMap<String, String>,
    home: Option<PathBuf>,
    working_directory: Option<&Path>,
) -> Option<(PathBuf, DataDirLocation)> {
    let (dir, location) =
        if let Some(dir) = arguments.and_then(|args| flag_value(args, "--data-dir")) {
            (PathBuf::from(dir), DataDirLocation::CommandLine)
        } else if let Some(dir) = environment.get(DATA_DIR_ENV).filter(|dir| !dir.is_empty()) {
            (PathBuf::from(dir), DataDirLocation::Environment)
        } else {
            (home?.join(".temps"), DataDirLocation::HomeDefault)
        };
    if dir.is_absolute() {
        Some((dir, location))
    } else {
        let base = working_directory.filter(|dir| dir.is_absolute())?;
        Some((base.join(dir).components().collect(), location))
    }
}

fn runs_serve(arguments: &[String]) -> bool {
    arguments.iter().skip(1).any(|arg| arg == "serve")
}

/// `--flag=value` or `--flag value`.
fn flag_value(arguments: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let mut iter = arguments.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(prefix.as_str()) {
            return Some(value.to_string());
        }
        if arg == flag {
            return iter.next().cloned();
        }
    }
    None
}

/// systemd's `%%` is a literal `%`. Other specifiers are left as written.
fn resolve_specifiers(value: &str) -> String {
    value.replace("%%", "%")
}

/// `~` or `~/x` against `home`; anything else unchanged when absolute.
fn expand_home(dir: &str, home: Option<&str>) -> Option<PathBuf> {
    if dir == "~" {
        return home.map(PathBuf::from);
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return home.map(|home| Path::new(home).join(rest));
    }
    let dir = PathBuf::from(dir);
    dir.is_absolute().then_some(dir)
}

/// Split a systemd value into words: whitespace separates, single and double
/// quotes group (anywhere in a word), backslash escapes the next character.
fn split_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = value.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            match (c, quote) {
                ('\\', _) => match chars.next() {
                    Some('n') => word.push('\n'),
                    Some('t') => word.push('\t'),
                    Some(other) => word.push(other),
                    None => {}
                },
                (c, Some(open)) if c == open => quote = None,
                ('"' | '\'', None) => quote = Some(c),
                (c, None) if c.is_whitespace() => break,
                (c, _) => word.push(c),
            }
        }
        words.push(word);
    }
    words
}

/// `${NAME}` and `$NAME` from the service environment; `$$` is a literal `$`.
/// An unset variable expands to nothing, as in systemd.
fn expand_variables(word: &str, environment: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('{') => {
                chars.next();
                let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                out.push_str(environment.get(&name).map(String::as_str).unwrap_or(""));
            }
            Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
                let mut name = String::new();
                while let Some(c) = chars
                    .peek()
                    .filter(|c| c.is_ascii_alphanumeric() || **c == '_')
                {
                    name.push(*c);
                    chars.next();
                }
                out.push_str(environment.get(&name).map(String::as_str).unwrap_or(""));
            }
            _ => out.push('$'),
        }
    }
    out
}

/// `KEY=VALUE` lines of an `EnvironmentFile=`: `#`/`;` comments, optional
/// single or double quotes around the value, `\` continuations.
fn parse_environment_file(contents: &str) -> Vec<(String, String)> {
    logical_lines(contents)
        .into_iter()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            let key = key.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return None;
            }
            let value = value.trim();
            let value = match split_words(value).as_slice() {
                [single] if value.starts_with(['"', '\'']) => single.clone(),
                _ => value.to_string(),
            };
            Some((key.to_string(), value))
        })
        .collect()
}

fn launchd_definition(agent: &Path, contents: &str, home: Option<&Path>) -> ServiceDefinition {
    let arguments = plist_value_after_key(contents, "ProgramArguments")
        .and_then(|rest| element_body(rest, "array"))
        .map(plist_strings);
    let mut environment = plist_value_after_key(contents, "EnvironmentVariables")
        .and_then(|rest| element_body(rest, "dict"))
        .map(plist_dict)
        .unwrap_or_default();
    // A launchd agent runs as the user whose LaunchAgents holds it.
    if let Some(home) = home {
        environment
            .entry("HOME".to_string())
            .or_insert_with(|| home.display().to_string());
    }
    let working_directory = plist_value_after_key(contents, "WorkingDirectory")
        .and_then(|rest| element_body(rest, "string"))
        .map(|dir| PathBuf::from(unescape_xml(dir.trim())))
        .filter(|dir| dir.is_absolute());

    let mut problems = Vec::new();
    let serve_arguments = arguments.as_deref().filter(|args| runs_serve(args));
    if arguments.is_some() && serve_arguments.is_none() {
        problems.push("ProgramArguments does not run `temps serve`".to_string());
    }
    let database_url = serve_database_url(serve_arguments, &environment, &[], None);
    let data_dir = serve_data_dir(
        serve_arguments,
        &environment,
        environment.get("HOME").map(PathBuf::from),
        working_directory.as_deref(),
    );
    ServiceDefinition {
        path: agent.to_path_buf(),
        kind: ServiceKind::Launchd,
        database_url,
        data_dir,
        working_directory,
        environment,
        problems,
    }
}

/// The text after `<key>name</key>`.
fn plist_value_after_key<'a>(contents: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("<key>{name}</key>");
    contents
        .find(&marker)
        .map(|index| &contents[index + marker.len()..])
}

/// The body of the first `<tag>...</tag>` in `rest`, which must come next.
fn element_body<'a>(rest: &'a str, tag: &str) -> Option<&'a str> {
    let rest = rest.trim_start().strip_prefix(&format!("<{tag}>"))?;
    let end = rest.find(&format!("</{tag}>"))?;
    Some(&rest[..end])
}

fn plist_strings(array: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut rest = array;
    while let Some(start) = rest.find("<string>") {
        let after = &rest[start + "<string>".len()..];
        let Some(end) = after.find("</string>") else {
            break;
        };
        strings.push(unescape_xml(&after[..end]));
        rest = &after[end + "</string>".len()..];
    }
    strings
}

fn plist_dict(dict: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut rest = dict;
    while let Some(start) = rest.find("<key>") {
        let after = &rest[start + "<key>".len()..];
        let Some(end) = after.find("</key>") else {
            break;
        };
        let key = unescape_xml(&after[..end]);
        let after = &after[end + "</key>".len()..];
        match element_body(after, "string") {
            Some(value) => {
                map.insert(key, unescape_xml(value));
                rest = after;
            }
            None => rest = after,
        }
    }
    map
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "postgresql://temps:s3cr3t-pw@127.0.0.1:5432/temps?sslmode=disable";

    /// The unit the installer writes, with generic paths and a made-up password.
    fn installer_unit(home: &Path) -> String {
        format!(
            "[Unit]\nDescription=Temps Platform Server\nAfter=network.target docker.service\n\n\
             [Service]\nType=simple\nUser=root\nWorkingDirectory={home}/.temps\n\
             ExecStart={home}/.temps/bin/temps serve \\\n  --address=\"0.0.0.0:80\" \\\n  \
             --tls-address=\"0.0.0.0:443\" \\\n  --database-url=\"{URL}\" \\\n  \
             --data-dir=\"{home}/.temps/data\" \\\n  --console-address=\"0.0.0.0:8081\"\n\
             Restart=always\nEnvironment=HOME={home}\n\n[Install]\nWantedBy=multi-user.target\n",
            home = home.display()
        )
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn found(probes: Vec<ServiceProbe>) -> ServiceDefinition {
        match probes.into_iter().next() {
            Some(ServiceProbe::Found(definition)) => *definition,
            other => panic!("expected a definition, got {other:?}"),
        }
    }

    fn systemd_at(unit: &Path) -> ServiceLocations {
        ServiceLocations {
            systemd_unit: Some(unit.to_path_buf()),
            launchd_agent: None,
        }
    }

    #[test]
    fn the_installer_unit_yields_its_url_and_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let unit = root.path().join("temps.service");
        write(&unit, &installer_unit(&home));

        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition
                .database_url
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .1,
            UrlLocation::CommandLine
        );
        assert_eq!(
            definition.data_dir,
            Some((home.join(".temps/data"), DataDirLocation::CommandLine))
        );
        assert_eq!(definition.working_directory, Some(home.join(".temps")));
        assert!(definition.problems.is_empty(), "{:?}", definition.problems);
    }

    #[test]
    fn an_environment_file_overrides_environment_and_is_named_as_the_origin() {
        let root = tempfile::tempdir().unwrap();
        let env_file = root.path().join("temps.env");
        write(
            &env_file,
            &format!("# database\nTEMPS_DATABASE_URL=\"{URL}\"\nTEMPS_DATA_DIR=/srv/temps\n"),
        );
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=\"TEMPS_DATABASE_URL=postgres://other/db\" HOME=/home/svc\n\
                 EnvironmentFile={}\nExecStart=/usr/local/bin/temps serve\n",
                env_file.display()
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition.database_url.unwrap().unwrap().1,
            UrlLocation::EnvironmentFile(env_file)
        );
        assert_eq!(
            definition.data_dir,
            Some((PathBuf::from("/srv/temps"), DataDirLocation::Environment))
        );
    }

    #[test]
    fn the_command_line_flag_wins_over_the_service_environment() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://env/db\n\
                 ExecStart=/opt/temps serve --database-url {URL}\n"
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
    }

    #[test]
    fn a_drop_in_replaces_exec_start_and_resets_environment() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            "[Service]\nEnvironment=TEMPS_DATABASE_URL=postgres://old/db\n\
             ExecStart=/usr/bin/temps serve --database-url=postgres://old/db\n",
        );
        write(
            &root.path().join("temps.service.d/override.conf"),
            &format!(
                "[Service]\nEnvironment=\nEnvironment=TEMPS_DATABASE_URL={URL}\n\
                 ExecStart=\nExecStart=/usr/bin/temps serve --data-dir /srv/data\n"
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition.database_url.unwrap().unwrap().1,
            UrlLocation::Environment
        );
        assert_eq!(
            definition.data_dir,
            Some((PathBuf::from("/srv/data"), DataDirLocation::CommandLine))
        );
    }

    #[test]
    fn a_systemd_credential_and_a_url_file_are_read() {
        let root = tempfile::tempdir().unwrap();
        let url_file = root.path().join("database-url");
        write(&url_file, &format!("{URL}\n"));
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nLoadCredential=temps_database_url:{}\nExecStart=/usr/bin/temps serve\n",
                url_file.display()
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition.database_url.unwrap().unwrap().1,
            UrlLocation::Credential(url_file.clone())
        );

        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE={}\nExecStart=/usr/bin/temps serve\n",
                url_file.display()
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(
            definition.database_url.unwrap().unwrap().1,
            UrlLocation::UrlFile(url_file)
        );
    }

    #[test]
    fn a_missing_url_file_is_an_error_not_a_silent_fallthrough() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            "[Service]\nEnvironment=TEMPS_DATABASE_URL_FILE=/nonexistent/database-url\n\
             LoadCredential=temps_database_url:/nonexistent/credential\n\
             ExecStart=/usr/bin/temps serve\n",
        );
        let definition = found(probe(&systemd_at(&unit)));
        let error = definition.database_url.unwrap_err();
        assert!(error.contains("/nonexistent/database-url"), "{error}");
    }

    #[test]
    fn a_unit_that_does_not_run_serve_supplies_no_url() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!("[Service]\nExecStart=/usr/bin/temps proxy --database-url={URL}\n"),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), None);
        assert!(definition
            .problems
            .iter()
            .any(|p| p.contains("temps serve")));
    }

    #[test]
    fn variables_in_exec_start_expand_from_the_service_environment() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=DB={URL} HOME=/home/svc\n\
                 ExecStart=-/usr/bin/temps serve --database-url=${{DB}} --data-dir $HOME/data\n"
            ),
        );
        let definition = found(probe(&systemd_at(&unit)));
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition.data_dir,
            Some((
                PathBuf::from("/home/svc/data"),
                DataDirLocation::CommandLine
            ))
        );
    }

    #[test]
    fn the_data_dir_defaults_to_the_service_home_and_resolves_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            "[Service]\nEnvironment=HOME=/home/svc\nExecStart=/usr/bin/temps serve\n",
        );
        assert_eq!(
            found(probe(&systemd_at(&unit))).data_dir,
            Some((
                PathBuf::from("/home/svc/.temps"),
                DataDirLocation::HomeDefault
            ))
        );

        write(
            &unit,
            "[Service]\nWorkingDirectory=/srv/temps\nExecStart=/usr/bin/temps serve --data-dir ./data\n",
        );
        assert_eq!(
            found(probe(&systemd_at(&unit))).data_dir,
            Some((
                PathBuf::from("/srv/temps/data"),
                DataDirLocation::CommandLine
            ))
        );
    }

    #[test]
    fn a_missing_unit_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let probes = probe(&systemd_at(&root.path().join("temps.service")));
        assert!(matches!(probes.as_slice(), [ServiceProbe::Absent { .. }]));
    }

    #[cfg(unix)]
    #[test]
    fn a_unit_other_users_can_rewrite_is_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(&unit, &installer_unit(root.path()));
        std::fs::set_permissions(&unit, std::fs::Permissions::from_mode(0o666)).unwrap();
        match probe(&systemd_at(&unit)).as_slice() {
            [ServiceProbe::Ignored { reason, .. }] => {
                assert!(reason.contains("writable by every user"), "{reason}")
            }
            other => panic!("expected the unit to be ignored, got {other:?}"),
        }
    }

    #[test]
    fn the_launchd_agent_yields_its_url_and_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let agent = home.join(LAUNCHD_AGENT_PATH);
        let escaped_url = URL.replace('&', "&amp;");
        write(
            &agent,
            &format!(
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n\
                 <key>Label</key>\n<string>dev.temps.temps</string>\n\
                 <key>ProgramArguments</key>\n<array>\n\
                 <string>{h}/.temps/bin/temps</string>\n<string>serve</string>\n\
                 <string>--database-url={escaped_url}</string>\n\
                 <string>--data-dir={h}/.temps/data</string>\n</array>\n\
                 <key>WorkingDirectory</key>\n<string>{h}/.temps</string>\n</dict>\n</plist>\n",
                h = home.display()
            ),
        );
        let definition = found(probe(&ServiceLocations {
            systemd_unit: None,
            launchd_agent: Some(agent),
        }));
        assert_eq!(definition.kind, ServiceKind::Launchd);
        assert_eq!(definition.url(), Some(URL));
        assert_eq!(
            definition.data_dir,
            Some((home.join(".temps/data"), DataDirLocation::CommandLine))
        );
        assert_eq!(definition.env("HOME"), Some(&*home.display().to_string()));
    }

    #[test]
    fn debug_output_never_contains_the_url_or_environment_values() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("temps.service");
        write(
            &unit,
            &format!(
                "[Service]\nEnvironment=TEMPS_AUTH_SECRET=hunter2-hunter2\n\
                 ExecStart=/usr/bin/temps serve --database-url={URL}\n"
            ),
        );
        let debug = format!("{:?}", found(probe(&systemd_at(&unit))));
        assert!(!debug.contains("s3cr3t-pw"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("TEMPS_AUTH_SECRET"), "{debug}");
    }

    #[test]
    fn words_split_like_systemd() {
        assert_eq!(
            split_words(r#"a "b c" --x="d e" 'f\'g' h\ i"#),
            ["a", "b c", "--x=d e", "f'g", "h i"]
        );
        assert_eq!(
            parse_environment_file("A=1\n; note\nB='two words'\nC=\"q\\\"d\"\n"),
            [
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string()),
                ("C".to_string(), "q\"d".to_string()),
            ]
        );
    }
}
