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
/// Where systemd mounts a unit's credentials (`%d`, `$CREDENTIALS_DIRECTORY`)
/// while it runs.
const SYSTEMD_CREDENTIALS_ROOT: &str = "/run/credentials";

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
    /// The data directory and where it was found; `Err` says why it is
    /// unknown.
    pub data_dir: Result<(PathBuf, DataDirLocation), String>,
    /// The server's working directory (`/` unless the definition sets one),
    /// where it also looks for GeoLite2 and against which it opens relative
    /// paths; `None` when the definition sets one that cannot be resolved.
    pub working_directory: Option<PathBuf>,
    /// The effective service environment (`Environment=` then
    /// `EnvironmentFile=`, which overrides it). `TEMPS_*_FILE` paths are
    /// absolute (resolved against the working directory) and point at the
    /// file `LoadCredential=` copies a credential from, since the server's
    /// credentials directory exists only inside the running service.
    pub environment: BTreeMap<String, String>,
    /// Why the environment the server sees is not fully known, for example
    /// an `EnvironmentFile=` this user cannot read. Empty when it is.
    pub environment_gaps: Vec<String>,
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
            .field("environment_gaps", &self.environment_gaps)
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

    /// Why the server's environment is not fully known, when it is not.
    pub(crate) fn incomplete_environment(&self) -> Option<String> {
        (!self.environment_gaps.is_empty()).then(|| self.environment_gaps.join("; "))
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
            sources.extend(read_drop_ins(unit, &drop_in_roots(unit))?);
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
    // SAFETY: geteuid cannot fail and touches no memory.
    let euid = unsafe { libc::geteuid() };
    let mode = metadata.mode();
    if metadata.uid() != 0 && metadata.uid() != euid {
        return Err(format!(
            "is owned by uid {}, neither root nor this user, so it is not trusted",
            metadata.uid()
        ));
    }
    if mode & 0o002 != 0 {
        return Err(
            "is writable by every user, so it is not trusted; remove that with `chmod o-w`"
                .to_string(),
        );
    }
    // Any member of the group could rewrite it, including the caller's own
    // group: the command may run as root, and the group may not be.
    if mode & 0o020 != 0 {
        return Err(format!(
            "is writable by group {}, so it is not trusted; remove that with `chmod g-w`",
            metadata.gid()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_not_writable_by_others(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// The directories systemd looks for drop-ins in, highest priority first
/// (systemd.unit(5)): `systemctl set-property` writes to `system.control`,
/// `systemctl edit --runtime` to `/run/systemd/system`, and packages ship
/// theirs under `/usr/lib`.
const SYSTEMD_DROP_IN_ROOTS: [&str; 9] = [
    "/etc/systemd/system.control",
    "/run/systemd/system.control",
    "/run/systemd/transient",
    "/etc/systemd/system",
    "/run/systemd/system",
    "/usr/local/lib/systemd/system",
    "/usr/lib/systemd/system",
    "/lib/systemd/system",
    "/run/systemd/generator.late",
];

/// Where to look for `unit`'s drop-ins: systemd's load paths for the unit
/// the installer writes, else (a unit somewhere else, as in tests) only its
/// own directory.
fn drop_in_roots(unit: &Path) -> Vec<PathBuf> {
    if unit == Path::new(SYSTEMD_UNIT_PATH) {
        SYSTEMD_DROP_IN_ROOTS.iter().map(PathBuf::from).collect()
    } else {
        unit.parent().map(Path::to_path_buf).into_iter().collect()
    }
}

/// `<root>/<unit>.d/*.conf` and `<root>/service.d/*.conf` across `roots`
/// (highest priority first), merged as systemd does: a file name in a
/// higher-priority directory hides the same name in a lower one, a drop-in
/// linked to `/dev/null` masks it, and the rest apply in lexical order of
/// file name.
fn read_drop_ins(unit: &Path, roots: &[PathBuf]) -> Result<Vec<(PathBuf, String)>, String> {
    let mut unit_dir = unit
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    unit_dir.push(".d");
    let mut chosen: BTreeMap<std::ffi::OsString, PathBuf> = BTreeMap::new();
    for root in roots {
        for dir in [root.join(&unit_dir), root.join("service.d")] {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "its drop-in directory {} cannot be read: {error}",
                        dir.display()
                    ))
                }
            };
            for path in entries.flatten().map(|entry| entry.path()) {
                if path.extension().is_some_and(|ext| ext == "conf") {
                    if let Some(name) = path.file_name() {
                        chosen.entry(name.to_os_string()).or_insert(path);
                    }
                }
            }
        }
    }
    let mut drop_ins = Vec::new();
    for path in chosen.into_values() {
        if std::fs::canonicalize(&path).is_ok_and(|target| target == Path::new("/dev/null")) {
            continue;
        }
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
    user: Option<String>,
    unset_environment: Vec<String>,
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
                "User" => {
                    service.user = (!value.is_empty()).then_some(value);
                    continue;
                }
                "Environment" => &mut service.environment,
                "EnvironmentFile" => &mut service.environment_files,
                "LoadCredential" => &mut service.load_credentials,
                "UnsetEnvironment" => &mut service.unset_environment,
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
    let credentials_dir = credentials_dir(unit);
    let specifiers = |value: &str| resolve_specifiers(value, &credentials_dir);
    let mut problems = Vec::new();
    let mut environment_gaps = Vec::new();

    // LoadCredential=<id>:<path>; the first entry for an id wins.
    let mut credentials: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in &service.load_credentials {
        let entry = specifiers(entry);
        if let Some((id, path)) = entry.split_once(':') {
            credentials
                .entry(id.trim().to_string())
                .or_insert_with(|| PathBuf::from(path.trim()));
        }
    }

    let mut environment = BTreeMap::new();
    for assignment in service
        .environment
        .iter()
        .flat_map(|value| split_words(&specifiers(value)))
    {
        if let Some((key, value)) = assignment.split_once('=') {
            environment.insert(key.to_string(), value.to_string());
        }
    }
    let environment_files: Vec<(bool, PathBuf)> = service
        .environment_files
        .iter()
        .map(|entry| {
            let entry = specifiers(entry);
            match entry.strip_prefix('-') {
                Some(path) => (true, PathBuf::from(path.trim())),
                None => (false, PathBuf::from(entry.trim())),
            }
        })
        .collect();
    // EnvironmentFile= overrides Environment=, later files override earlier.
    for (optional, path) in &environment_files {
        match read_trusted(path) {
            Ok(Some(contents)) => {
                for (key, value) in parse_environment_file(&contents) {
                    environment.insert(key, value);
                }
            }
            Ok(None) if *optional => {}
            Ok(None) => problems.push(format!(
                "EnvironmentFile={} does not exist (systemd refuses to start the unit)",
                path.display()
            )),
            // The server reads it, so whatever it sets is unknown here.
            Err(reason) => {
                let gap = format!("EnvironmentFile={} {reason}", path.display());
                problems.push(gap.clone());
                environment_gaps.push(gap);
            }
        }
    }
    // `%d/<id>` exists only inside the running service: read the file
    // LoadCredential= copies it from instead.
    for value in environment.values_mut() {
        let source = Path::new(value.as_str())
            .strip_prefix(&credentials_dir)
            .ok()
            .and_then(Path::to_str)
            .and_then(|id| credentials.get(id))
            .filter(|source| source.is_absolute())
            .cloned();
        if let Some(source) = source {
            *value = source.display().to_string();
        }
    }

    // systemd sets HOME for a service with User= (Environment= can override
    // it). Without User= the server runs as root with no HOME, and
    // `dirs::home_dir` falls back to root's account.
    let user = service.user.as_deref().map(specifiers);
    if let Some(user) = &user {
        if !environment.contains_key("HOME") {
            if let Some(home) = account_home(user) {
                environment.insert("HOME".to_string(), home.display().to_string());
            }
        }
    }
    let home = match environment.get("HOME").filter(|home| !home.is_empty()) {
        Some(home) => Ok(PathBuf::from(home)),
        None => match &user {
            Some(user) => Err(format!(
                "the home directory of its User={user} could not be found"
            )),
            None => account_home("0")
                .ok_or_else(|| "root's home directory could not be found".to_string()),
        },
    };
    // UnsetEnvironment= is applied last, to the whole environment: `NAME`
    // removes the variable, `NAME=VALUE` only that exact assignment. A HOME
    // it removes still leaves `dirs::home_dir` the account's.
    for entry in service
        .unset_environment
        .iter()
        .flat_map(|value| split_words(&specifiers(value)))
    {
        match entry.split_once('=') {
            Some((key, value)) => {
                if environment.get(key).is_some_and(|set| set == value) {
                    environment.remove(key);
                }
            }
            None => {
                environment.remove(&entry);
            }
        }
    }

    let cwd = match service.working_directory.as_deref().map(specifiers) {
        None => Ok(PathBuf::from("/")),
        Some(dir) => {
            let dir = dir.strip_prefix('-').unwrap_or(&dir);
            expand_home(dir, home.as_deref().ok()).ok_or_else(|| {
                format!("the service's WorkingDirectory={dir} could not be resolved")
            })
        }
    };
    let file_gaps = resolve_file_variables(&mut environment, &cwd);

    let mut referenced = Vec::new();
    let arguments = service.exec_start.as_deref().map(|exec_start| {
        let exec_start = exec_start.trim_start_matches(['@', '-', ':', '+', '!']);
        split_words(&specifiers(exec_start))
            .into_iter()
            .map(|word| expand_variables(&word, &environment, &mut referenced))
            .collect::<Vec<_>>()
    });
    let serve_arguments = arguments.as_deref().filter(|args| runs_serve(args));
    if arguments.is_some() && serve_arguments.is_none() {
        problems.push("ExecStart= does not run `temps serve`".to_string());
    }

    let environment_gap = (!environment_gaps.is_empty()).then(|| environment_gaps.join("; "));
    let (database_url, data_dir) = match &environment_gap {
        // ExecStart= expands variables, and the unreadable file, which
        // systemd applies after Environment=, may set or override any of them.
        Some(gap) if serve_arguments.is_some() && !referenced.is_empty() => {
            let reason = format!(
                "ExecStart= uses ${} and {gap}; that file may set them, so what `temps serve` \
                 runs with is unknown",
                referenced.join(", $")
            );
            (Err(reason.clone()), Err(reason))
        }
        _ => (
            serve_database_url(
                serve_arguments,
                &environment,
                environment_gap.as_deref(),
                &environment_files,
                credentials.get(DATABASE_URL_CREDENTIAL).cloned(),
                &cwd,
            ),
            serve_data_dir(
                serve_arguments,
                &environment,
                environment_gap.as_deref(),
                home,
                &cwd,
            ),
        ),
    };
    environment_gaps.extend(file_gaps);

    ServiceDefinition {
        path: unit.to_path_buf(),
        kind: ServiceKind::Systemd,
        database_url,
        data_dir,
        working_directory: cwd.ok(),
        environment,
        environment_gaps,
        problems,
    }
}

/// The home directory of the account `user` (a name or a numeric uid), as
/// the server's `dirs::home_dir` finds it when HOME is unset.
#[cfg(unix)]
fn account_home(user: &str) -> Option<PathBuf> {
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: `passwd` is plain old data; all-zero is a valid value that
    // getpw*_r overwrites.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = match user.parse::<libc::uid_t>() {
        // SAFETY: every pointer is valid for the call and `buffer.len()` is
        // the buffer's real size.
        Ok(uid) => unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        },
        Err(_) => {
            let name = CString::new(user).ok()?;
            // SAFETY: as above; `name` is NUL-terminated and outlives the call.
            unsafe {
                libc::getpwnam_r(
                    name.as_ptr(),
                    &mut entry,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut result,
                )
            }
        }
    };
    if status != 0 || result.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpw*_r succeeded, so `pw_dir` points at a NUL-terminated
    // string inside `buffer`, which is still alive.
    let dir = unsafe { CStr::from_ptr(entry.pw_dir) };
    let dir = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
    dir.is_absolute().then_some(dir)
}

#[cfg(not(unix))]
fn account_home(_user: &str) -> Option<PathBuf> {
    None
}

/// `%d` for `unit`: `/run/credentials/<unit name>`.
fn credentials_dir(unit: &Path) -> PathBuf {
    let name = unit
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "temps.service".to_string());
    Path::new(SYSTEMD_CREDENTIALS_ROOT).join(name)
}

/// `path` as the server opens it: relative to its working directory.
fn absolute_in(path: PathBuf, cwd: &Result<PathBuf, String>) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path);
    }
    match cwd {
        Ok(cwd) => Ok(cwd.join(&path).components().collect()),
        Err(reason) => Err(format!("{} is relative, and {reason}", path.display())),
    }
}

/// `serve` opens a relative `TEMPS_*_FILE` against its own working
/// directory, not this command's, so make each absolute. Returns the ones
/// that cannot be (other than the URL file, whose error the URL carries).
fn resolve_file_variables(
    environment: &mut BTreeMap<String, String>,
    cwd: &Result<PathBuf, String>,
) -> Vec<String> {
    let mut gaps = Vec::new();
    for (key, value) in environment
        .iter_mut()
        .filter(|(key, _)| key.starts_with("TEMPS_") && key.ends_with("_FILE"))
    {
        let raw = value.trim();
        if raw.is_empty() {
            continue;
        }
        match absolute_in(PathBuf::from(raw), cwd) {
            Ok(path) => *value = path.display().to_string(),
            Err(reason) if key != DATABASE_URL_FILE_ENV => gaps.push(format!("{key}={reason}")),
            Err(_) => {}
        }
    }
    gaps
}

/// `serve` with `--database-url` wins over `TEMPS_DATABASE_URL`, which wins
/// over `TEMPS_DATABASE_URL_FILE`, which wins over the credential: the same
/// order as `temps serve` itself (clap, then `adopt_database_url_file`).
/// With `environment_gap`, only the flag is certain.
fn serve_database_url(
    arguments: Option<&[String]>,
    environment: &BTreeMap<String, String>,
    environment_gap: Option<&str>,
    environment_files: &[(bool, PathBuf)],
    credential: Option<PathBuf>,
    cwd: &Result<PathBuf, String>,
) -> Result<Option<(String, UrlLocation)>, String> {
    if let Some(url) = arguments.and_then(|args| flag_value(args, "--database-url")) {
        return Ok(Some((url, UrlLocation::CommandLine)));
    }
    if let Some(gap) = environment_gap {
        return Err(format!(
            "{gap}; it may set {DATABASE_URL_ENV}, so the URL the server uses is unknown"
        ));
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
        let path = absolute_in(PathBuf::from(path.trim()), cwd)
            .map_err(|reason| format!("{DATABASE_URL_FILE_ENV}={reason}"))?;
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
fn environment_file_defining(environment_files: &[(bool, PathBuf)], name: &str) -> Option<PathBuf> {
    environment_files.iter().rev().find_map(|(_, path)| {
        let contents = read_trusted(path).ok().flatten()?;
        parse_environment_file(&contents)
            .into_iter()
            .any(|(key, _)| key == name)
            .then(|| path.clone())
    })
}

/// `serve`'s data directory: `--data-dir`, `TEMPS_DATA_DIR`, then
/// `$HOME/.temps`. A relative path is relative to the working directory.
/// With `environment_gap`, only the flag is certain.
fn serve_data_dir(
    arguments: Option<&[String]>,
    environment: &BTreeMap<String, String>,
    environment_gap: Option<&str>,
    home: Result<PathBuf, String>,
    cwd: &Result<PathBuf, String>,
) -> Result<(PathBuf, DataDirLocation), String> {
    let (dir, location) =
        if let Some(dir) = arguments.and_then(|args| flag_value(args, "--data-dir")) {
            (PathBuf::from(dir), DataDirLocation::CommandLine)
        } else if let Some(gap) = environment_gap {
            return Err(format!(
                "{gap}; it may set {DATA_DIR_ENV}, so the data directory is unknown"
            ));
        } else if let Some(dir) = environment.get(DATA_DIR_ENV).filter(|dir| !dir.is_empty()) {
            (PathBuf::from(dir), DataDirLocation::Environment)
        } else {
            match home {
                Ok(home) => (home.join(".temps"), DataDirLocation::HomeDefault),
                Err(reason) => {
                    return Err(format!(
                        "it sets no --data-dir or {DATA_DIR_ENV}, and {reason}, so the data \
                         directory is unknown"
                    ))
                }
            }
        };
    let dir = absolute_in(dir, cwd).map_err(|reason| format!("its data directory {reason}"))?;
    Ok((dir, location))
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

/// systemd's `%%` (a literal `%`) and `%d` (the unit's credentials
/// directory). Other specifiers are left as written.
fn resolve_specifiers(value: &str, credentials_dir: &Path) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('d') => out.push_str(&credentials_dir.display().to_string()),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `~` or `~/x` against `home`; anything else unchanged when absolute.
fn expand_home(dir: &str, home: Option<&Path>) -> Option<PathBuf> {
    if dir == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
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
/// An unset variable expands to nothing, as in systemd. Every name used is
/// added to `referenced`.
fn expand_variables(
    word: &str,
    environment: &BTreeMap<String, String>,
    referenced: &mut Vec<String>,
) -> String {
    let mut lookup = |name: String| {
        let value = environment.get(&name).cloned().unwrap_or_default();
        if !referenced.contains(&name) {
            referenced.push(name);
        }
        value
    };
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
                out.push_str(&lookup(name));
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
                out.push_str(&lookup(name));
            }
            _ => out.push('$'),
        }
    }
    out
}

/// The assignments of an `EnvironmentFile=`, read with systemd's own rules
/// for that format (`parse_env_file`), which differ from unit files':
/// - `#`/`;` start a comment only at the start of a line;
/// - unquoted, `\x` is `x`, a trailing `\` joins the next line (with no
///   space), and trailing whitespace is dropped;
/// - `'...'` is literal;
/// - in `"..."` only `\"`, `\\`, `` \` `` and `\$` are escapes, others keep
///   their backslash.
fn parse_environment_file(contents: &str) -> Vec<(String, String)> {
    #[derive(Clone, Copy)]
    enum State {
        PreKey,
        Key,
        PreValue,
        Value,
        ValueEscape,
        Single,
        Double,
        DoubleEscape,
        Comment,
        CommentEscape,
    }
    let mut assignments = Vec::new();
    let mut state = State::PreKey;
    let mut key = String::new();
    let mut value = String::new();
    // Length of `value` up to its last character that is not unquoted,
    // unescaped trailing whitespace.
    let mut significant = 0;
    let mut finish = |key: &mut String, value: &mut String, significant: &mut usize| {
        value.truncate(*significant);
        let name = key.trim_end();
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            assignments.push((name.to_string(), std::mem::take(value)));
        }
        key.clear();
        value.clear();
        *significant = 0;
    };
    for c in contents.chars() {
        state = match (state, c) {
            (State::PreKey, '#' | ';') => State::Comment,
            (State::PreKey, c) if c.is_whitespace() => State::PreKey,
            (State::PreKey, c) => {
                key.push(c);
                State::Key
            }
            (State::Key, '\n') => {
                key.clear();
                State::PreKey
            }
            (State::Key, '=') => State::PreValue,
            (State::Key, c) => {
                key.push(c);
                State::Key
            }
            (State::PreValue | State::Value, '\n') => {
                finish(&mut key, &mut value, &mut significant);
                State::PreKey
            }
            (State::PreValue, '\'') => State::Single,
            (State::PreValue, '"') => State::Double,
            (State::PreValue | State::Value, '\\') => State::ValueEscape,
            (State::PreValue, c) if c.is_whitespace() => State::PreValue,
            (State::PreValue | State::Value, c) => {
                value.push(c);
                if !c.is_whitespace() {
                    significant = value.len();
                }
                State::Value
            }
            (State::ValueEscape, '\n') => State::Value,
            (State::ValueEscape, c) => {
                value.push(c);
                significant = value.len();
                State::Value
            }
            (State::Single, '\'') => State::PreValue,
            (State::Single, c) => {
                value.push(c);
                significant = value.len();
                State::Single
            }
            (State::Double, '"') => State::PreValue,
            (State::Double, '\\') => State::DoubleEscape,
            (State::Double, c) => {
                value.push(c);
                significant = value.len();
                State::Double
            }
            (State::DoubleEscape, '\n') => State::Double,
            (State::DoubleEscape, c) => {
                if !matches!(c, '"' | '\\' | '`' | '$') {
                    value.push('\\');
                }
                value.push(c);
                significant = value.len();
                State::Double
            }
            (State::Comment, '\\') => State::CommentEscape,
            (State::Comment, '\n') => State::PreKey,
            (State::Comment, _) | (State::CommentEscape, _) => State::Comment,
        };
    }
    match state {
        State::PreValue
        | State::Value
        | State::ValueEscape
        | State::Single
        | State::Double
        | State::DoubleEscape => finish(&mut key, &mut value, &mut significant),
        State::PreKey | State::Key | State::Comment | State::CommentEscape => {}
    }
    assignments
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
    // launchd starts a job in `/` unless WorkingDirectory says otherwise.
    let cwd = match plist_value_after_key(contents, "WorkingDirectory")
        .and_then(|rest| element_body(rest, "string"))
        .map(|dir| unescape_xml(dir.trim()))
    {
        None => Ok(PathBuf::from("/")),
        Some(dir) if Path::new(&dir).is_absolute() => Ok(PathBuf::from(dir)),
        Some(dir) => Err(format!(
            "the agent's WorkingDirectory {dir} is not an absolute path"
        )),
    };
    let environment_gaps = resolve_file_variables(&mut environment, &cwd);

    let mut problems = Vec::new();
    let serve_arguments = arguments.as_deref().filter(|args| runs_serve(args));
    if arguments.is_some() && serve_arguments.is_none() {
        problems.push("ProgramArguments does not run `temps serve`".to_string());
    }
    let database_url = serve_database_url(serve_arguments, &environment, None, &[], None, &cwd);
    let home = environment
        .get("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "the agent's HOME is unknown".to_string());
    let data_dir = serve_data_dir(serve_arguments, &environment, None, home, &cwd);
    ServiceDefinition {
        path: agent.to_path_buf(),
        kind: ServiceKind::Launchd,
        database_url,
        data_dir,
        working_directory: cwd.ok(),
        environment,
        environment_gaps,
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

/// Fixtures for tests that probe a service definition.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    /// `std::fs::write`, creating the parent directories, with mode 0644
    /// whatever the umask: under umask 002 a plain write is group-writable,
    /// and the probe rightly ignores it.
    pub(crate) fn write_trusted(
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> std::io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
