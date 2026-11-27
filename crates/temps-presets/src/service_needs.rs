// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which managed database a repository needs, and which of its
//! `.env.example` keys a linked database fills.
//!
//! Detection reads what a repository already says about itself: the keys of
//! its env-example file, the dependencies in its manifests (`package.json`,
//! `requirements.txt`, `pyproject.toml`, `Gemfile`, `go.mod`), a Prisma
//! schema's provider, and the images of a committed Compose file. Each
//! suggestion carries its evidence, so the console can say why in one line
//! ("`.env.example` asks for `DATABASE_URL`, and `package.json` uses `pg`").
//!
//! The variable names a linked database injects (`POSTGRES_URL`, ...) rarely
//! match what an app asks for (`DATABASE_URL`). [`env_aliases`] maps the app's
//! names onto the injected ones, so the link can inject both.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A managed database Temps can create, by its service type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseKind {
    Postgres,
    /// MySQL-compatible, served by MariaDB.
    Mariadb,
    Mongodb,
    Redis,
}

impl DatabaseKind {
    pub const ALL: [DatabaseKind; 4] = [
        DatabaseKind::Postgres,
        DatabaseKind::Mariadb,
        DatabaseKind::Mongodb,
        DatabaseKind::Redis,
    ];

    /// The `external_services.service_type` Temps creates for it.
    pub fn service_type(self) -> &'static str {
        match self {
            DatabaseKind::Postgres => "postgres",
            DatabaseKind::Mariadb => "mariadb",
            DatabaseKind::Mongodb => "mongodb",
            DatabaseKind::Redis => "redis",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DatabaseKind::Postgres => "Postgres",
            DatabaseKind::Mariadb => "MySQL (MariaDB)",
            DatabaseKind::Mongodb => "MongoDB",
            DatabaseKind::Redis => "Redis",
        }
    }

    /// The variables a linked database of this kind injects. Kept in step
    /// with the providers' `get_runtime_env_vars`.
    pub fn injected(self) -> &'static [&'static str] {
        match self {
            DatabaseKind::Postgres => &[
                "POSTGRES_URL",
                "POSTGRES_HOST",
                "POSTGRES_PORT",
                "POSTGRES_DB",
                "POSTGRES_NAME",
                "POSTGRES_USER",
                "POSTGRES_PASSWORD",
            ],
            DatabaseKind::Mariadb => &[
                "DATABASE_URL",
                "MYSQL_HOST",
                "MYSQL_PORT",
                "MYSQL_DATABASE",
                "MYSQL_USER",
                "MYSQL_PASSWORD",
                "MARIADB_HOST",
                "MARIADB_PORT",
                "MARIADB_DATABASE",
                "MARIADB_USER",
                "MARIADB_PASSWORD",
            ],
            DatabaseKind::Mongodb => &[
                "MONGODB_URL",
                "MONGODB_HOST",
                "MONGODB_PORT",
                "MONGODB_DATABASE",
                "MONGODB_USERNAME",
                "MONGODB_PASSWORD",
            ],
            DatabaseKind::Redis => &[
                "REDIS_URL",
                "REDIS_HOST",
                "REDIS_PORT",
                "REDIS_PASSWORD",
                "REDIS_DATABASE",
            ],
        }
    }

    /// Names apps commonly use for what this kind injects under another name.
    fn common_aliases(self) -> &'static [(&'static str, &'static str)] {
        match self {
            DatabaseKind::Postgres => &[
                ("DATABASE_URL", "POSTGRES_URL"),
                ("POSTGRES_URI", "POSTGRES_URL"),
                ("POSTGRES_DATABASE", "POSTGRES_DB"),
                ("PGHOST", "POSTGRES_HOST"),
                ("PGPORT", "POSTGRES_PORT"),
                ("PGUSER", "POSTGRES_USER"),
                ("PGPASSWORD", "POSTGRES_PASSWORD"),
                ("PGDATABASE", "POSTGRES_DB"),
            ],
            DatabaseKind::Mariadb => &[
                ("MYSQL_URL", "DATABASE_URL"),
                ("MARIADB_URL", "DATABASE_URL"),
            ],
            DatabaseKind::Mongodb => &[
                ("MONGODB_URI", "MONGODB_URL"),
                ("MONGO_URL", "MONGODB_URL"),
                ("MONGO_URI", "MONGODB_URL"),
                ("DATABASE_URL", "MONGODB_URL"),
            ],
            DatabaseKind::Redis => &[("REDIS_URI", "REDIS_URL"), ("KV_URL", "REDIS_URL")],
        }
    }
}

/// A database the repository appears to need, with the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedService {
    pub kind: DatabaseKind,
    /// One line naming the strongest evidence.
    pub reason: String,
    /// Each piece of evidence: `.env.example asks for DATABASE_URL`.
    pub evidence: Vec<String>,
}

/// A file read from the repository for detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
    pub path: String,
    pub content: String,
}

/// Manifest files detection reads, at the project root.
pub const MANIFEST_FILES: &[&str] = &[
    "package.json",
    "requirements.txt",
    "pyproject.toml",
    "Gemfile",
    "go.mod",
    "prisma/schema.prisma",
];

/// The kind a URL-shaped example value points at (`postgres://...`). A
/// driver after `+` names how to connect, not the database
/// (SQLAlchemy's `postgresql+asyncpg://`, `mysql+pymysql://`, and
/// `mongodb+srv://`).
fn kind_from_url(value: &str) -> Option<DatabaseKind> {
    let scheme = value.trim().split_once("://")?.0.to_ascii_lowercase();
    let database = scheme.split('+').next().unwrap_or_default();
    match database {
        "postgres" | "postgresql" => Some(DatabaseKind::Postgres),
        "mysql" | "mariadb" => Some(DatabaseKind::Mariadb),
        "mongodb" => Some(DatabaseKind::Mongodb),
        "redis" | "rediss" => Some(DatabaseKind::Redis),
        _ => None,
    }
}

/// What an env-example key says about the database, from its name and its
/// example value.
fn kind_from_env(key: &str, example: &str) -> Option<DatabaseKind> {
    if let Some(kind) = kind_from_url(example) {
        return Some(kind);
    }
    let k = key.to_ascii_uppercase();
    if k.starts_with("POSTGRES_") || k.starts_with("PG") && k.len() > 2 && is_pg_var(&k) {
        Some(DatabaseKind::Postgres)
    } else if k.starts_with("MYSQL_") || k.starts_with("MARIADB_") {
        Some(DatabaseKind::Mariadb)
    } else if k.starts_with("MONGODB_") || k.starts_with("MONGO_") {
        Some(DatabaseKind::Mongodb)
    } else if k.starts_with("REDIS_") {
        Some(DatabaseKind::Redis)
    } else {
        None
    }
}

fn is_pg_var(key: &str) -> bool {
    matches!(
        key,
        "PGHOST" | "PGPORT" | "PGUSER" | "PGPASSWORD" | "PGDATABASE"
    )
}

/// Dependency names and the database each means, per manifest.
fn dependency_kind(manifest: &str, name: &str) -> Option<DatabaseKind> {
    let name = name.trim().to_ascii_lowercase();
    let name = name.as_str();
    match manifest {
        "package.json" => match name {
            "pg" | "postgres" | "pg-promise" | "@neondatabase/serverless" | "@vercel/postgres" => {
                Some(DatabaseKind::Postgres)
            }
            "mysql" | "mysql2" | "mariadb" => Some(DatabaseKind::Mariadb),
            "mongoose" | "mongodb" => Some(DatabaseKind::Mongodb),
            "ioredis" | "redis" | "bullmq" | "bull" => Some(DatabaseKind::Redis),
            _ => None,
        },
        "requirements.txt" | "pyproject.toml" => {
            // `psycopg[binary]` is psycopg: extras pick optional parts of a
            // package, and names compare with `_` and `.` as `-` (PEP 503).
            let (base, extras) = match name.split_once('[') {
                Some((base, rest)) => (base, rest.trim_end_matches(']')),
                None => (name, ""),
            };
            let base = base.trim().replace(['_', '.'], "-");
            let has_extra = |wanted: &str| extras.split(',').any(|e| e.trim() == wanted);
            match base.as_str() {
                "psycopg2" | "psycopg2-binary" | "psycopg" | "psycopg-binary" | "asyncpg" => {
                    Some(DatabaseKind::Postgres)
                }
                "mysqlclient" | "pymysql" | "mysql-connector-python" | "aiomysql" => {
                    Some(DatabaseKind::Mariadb)
                }
                "pymongo" | "motor" | "mongoengine" => Some(DatabaseKind::Mongodb),
                "redis" | "rq" => Some(DatabaseKind::Redis),
                "celery" if has_extra("redis") => Some(DatabaseKind::Redis),
                _ => None,
            }
        }
        "Gemfile" => match name {
            "pg" => Some(DatabaseKind::Postgres),
            "mysql2" | "trilogy" => Some(DatabaseKind::Mariadb),
            "mongoid" => Some(DatabaseKind::Mongodb),
            "redis" | "sidekiq" => Some(DatabaseKind::Redis),
            _ => None,
        },
        "go.mod" => match name {
            "github.com/lib/pq" | "github.com/jackc/pgx/v5" | "github.com/jackc/pgx/v4" => {
                Some(DatabaseKind::Postgres)
            }
            "github.com/go-sql-driver/mysql" => Some(DatabaseKind::Mariadb),
            "go.mongodb.org/mongo-driver" | "go.mongodb.org/mongo-driver/v2" => {
                Some(DatabaseKind::Mongodb)
            }
            "github.com/redis/go-redis/v9" | "github.com/go-redis/redis/v8" => {
                Some(DatabaseKind::Redis)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The dependency names a manifest declares. Tolerant: a file that does not
/// parse yields nothing rather than an error.
fn dependencies(path: &str, content: &str) -> Vec<String> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file {
        "package.json" => {
            let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
                return Vec::new();
            };
            ["dependencies", "devDependencies", "optionalDependencies"]
                .iter()
                .filter_map(|section| json.get(section).and_then(|v| v.as_object()))
                .flat_map(|deps| deps.keys().cloned())
                .collect()
        }
        "requirements.txt" => content
            .lines()
            .map(|l| l.split('#').next().unwrap_or_default().trim())
            .filter(|l| !l.is_empty() && !l.starts_with('-'))
            .map(|l| {
                // The name, then its extras (`psycopg [binary]`) kept whole.
                let end = l
                    .find(['=', '<', '>', '~', '!', ';', ' ', '\t', '[', '@'])
                    .unwrap_or(l.len());
                let (name, rest) = l.split_at(end);
                match rest
                    .trim_start()
                    .strip_prefix('[')
                    .and_then(|r| r.split_once(']'))
                {
                    Some((extras, _)) => format!("{name}[{extras}]"),
                    None => name.to_string(),
                }
            })
            .collect(),
        "pyproject.toml" => pyproject_dependencies(content),
        "Gemfile" => content
            .lines()
            .filter_map(|l| {
                let rest = l.trim().strip_prefix("gem ")?;
                let quote = rest.chars().next()?;
                if quote != '"' && quote != '\'' {
                    return None;
                }
                rest[1..].split(quote).next().map(str::to_string)
            })
            .collect(),
        "go.mod" => content
            .lines()
            .map(|l| l.trim().trim_start_matches("require").trim())
            .filter_map(|l| l.split_whitespace().next())
            .filter(|m| m.contains('/'))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The provider a Prisma schema's datasource names: the quoted string
/// after `provider =`, whatever follows it (a `//` comment).
fn prisma_provider_name(content: &str) -> Option<String> {
    let mut in_datasource = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("datasource ") {
            in_datasource = true;
        } else if in_datasource && line.starts_with('}') {
            in_datasource = false;
        } else if in_datasource {
            if let Some(value) = line.strip_prefix("provider") {
                let Some(value) = value.trim().strip_prefix('=') else {
                    continue;
                };
                let value = value.trim();
                let value = match value.strip_prefix('"') {
                    Some(quoted) => quoted.split('"').next().unwrap_or_default(),
                    None => value
                        .split("//")
                        .next()
                        .unwrap_or_default()
                        .split_whitespace()
                        .next()
                        .unwrap_or_default(),
                };
                return Some(value.trim().to_ascii_lowercase());
            }
        }
    }
    None
}

/// The database a Prisma provider name is, when Temps offers it.
fn prisma_kind(provider: &str) -> Option<DatabaseKind> {
    match provider {
        // CockroachDB speaks Postgres's protocol but is another engine, whose
        // migrations (`unique_rowid()`, `crdb_internal`) Postgres refuses:
        // Temps offers none, so nothing is suggested for it.
        "postgresql" | "postgres" => Some(DatabaseKind::Postgres),
        "mysql" => Some(DatabaseKind::Mariadb),
        "mongodb" => Some(DatabaseKind::Mongodb),
        _ => None,
    }
}

/// Whether a connection string example names a database of its own
/// (`file:./dev.db`, `sqlserver://…`): one Temps does not offer, which a
/// bare DATABASE_URL must not be taken to mean Postgres for. An empty
/// value or a placeholder names none.
fn names_another_database(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    kind_from_url(value).is_none()
        && (value.contains("://") || lower.starts_with("file:") || lower.starts_with("sqlite:"))
}

/// The packages a `pyproject.toml` depends on: the quoted requirements in
/// the arrays of `[project]` (`dependencies`), `[project.optional-dependencies]`
/// and `[dependency-groups]`, inline or over several lines, and the keys of
/// Poetry's dependency tables. Nothing else in the file is a dependency.
fn pyproject_dependencies(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut in_array = false;
    // The name with its extras (`celery[redis]`), which can pick a service
    // as requirements.txt's do.
    let requirement_name = |requirement: &str| {
        let requirement = requirement.trim();
        let end = requirement
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(requirement.len());
        if end == 0 {
            return None;
        }
        let name = &requirement[..end];
        let rest = requirement[end..].trim_start();
        match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((extras, _)) => Some(format!("{name}[{extras}]")),
            None => Some(name.to_string()),
        }
    };
    // The requirements quoted in `text`, and whether it closes the array.
    let quoted = |text: &str, out: &mut Vec<String>| {
        let (strings, closes) = toml_array_strings(text);
        out.extend(strings.iter().filter_map(|s| requirement_name(s)));
        closes
    };
    for raw in content.lines() {
        if in_array {
            in_array = !quoted(raw, &mut out);
            continue;
        }
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') && !line.contains('=') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let value = value.trim();
        let poetry_table = section.starts_with("tool.poetry")
            && section.ends_with("dependencies");
        let array_section = (section == "project" && key == "dependencies")
            || section == "project.optional-dependencies"
            || section == "dependency-groups";
        if poetry_table {
            if key != "python" {
                out.extend(requirement_name(key));
            }
        } else if array_section && value.starts_with('[') {
            let value = raw.split_once('=').map_or("", |(_, v)| v);
            in_array = !quoted(value, &mut out);
        }
    }
    out
}

/// The strings in one line of a TOML array, in double or single quotes,
/// and whether the line closes the array: a `]` outside quotes (an extra
/// such as `psycopg[binary]` is inside them). A `#` outside quotes starts
/// a comment. Strings in an inline table are no requirement: in
/// `{ include-group = "redis" }` the string names a dependency group.
fn toml_array_strings(line: &str) -> (Vec<String>, bool) {
    let mut strings = Vec::new();
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut table_depth = 0usize;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => {
                let string = std::mem::take(&mut current);
                if table_depth == 0 {
                    strings.push(string);
                }
                quote = None;
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => break,
            None if c == '{' => table_depth += 1,
            None if c == '}' => table_depth = table_depth.saturating_sub(1),
            None if c == ']' && table_depth == 0 => return (strings, true),
            None => {}
        }
    }
    (strings, false)
}

/// The database a Compose service image is.
pub fn kind_from_image(image: &str) -> Option<DatabaseKind> {
    let name = image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split([':', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "postgres" | "postgis" | "timescaledb" | "timescaledb-ha" | "pgvector" => {
            Some(DatabaseKind::Postgres)
        }
        "mysql" | "mariadb" | "percona" => Some(DatabaseKind::Mariadb),
        "mongo" | "mongodb" => Some(DatabaseKind::Mongodb),
        "redis" | "valkey" | "redis-stack" | "keydb" => Some(DatabaseKind::Redis),
        _ => None,
    }
}

/// The databases a repository appears to need, strongest first.
///
/// `env` is the env-example's `(key, example value)` pairs; `files` the
/// manifests and Prisma schema read from the project root; `compose_images`
/// the images of a committed Compose file's services.
pub fn suggest_services(
    env: &[(String, String)],
    files: &[RepoFile],
    compose_images: &[String],
) -> Vec<SuggestedService> {
    let mut evidence: BTreeMap<DatabaseKind, Vec<String>> = BTreeMap::new();

    // A Prisma schema names its provider; that decides what a bare
    // DATABASE_URL means.
    let prisma_provider = files
        .iter()
        .find(|f| f.path.ends_with("schema.prisma"))
        .and_then(|f| prisma_provider_name(&f.content));
    let prisma = prisma_provider.as_deref().and_then(prisma_kind);

    for (key, example) in env {
        let kind = kind_from_env(key, example).or_else(|| {
            (key.eq_ignore_ascii_case("DATABASE_URL"))
                .then_some(prisma)
                .flatten()
        });
        if let Some(kind) = kind {
            push(&mut evidence, kind, format!("`.env.example` asks for `{key}`"));
        }
    }
    for file in files {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        if name == "schema.prisma" {
            if let Some(kind) = prisma {
                push(&mut evidence, kind, format!("`{}` uses the {} provider", file.path, kind.label()));
            }
            continue;
        }
        for dep in dependencies(&file.path, &file.content) {
            if let Some(kind) = dependency_kind(name, &dep) {
                push(&mut evidence, kind, format!("`{}` uses `{dep}`", file.path));
            }
        }
    }
    for image in compose_images {
        if let Some(kind) = kind_from_image(image) {
            push(&mut evidence, kind, format!("Compose runs `{image}`"));
        }
    }
    // A DATABASE_URL whose example does not say which database it is goes to
    // the one SQL or document database the rest of the evidence names; with
    // none, to Postgres, the most common ask.
    // A schema or example that names a database Temps does not offer
    // (SQLite, SQL Server) is not a bare DATABASE_URL: nothing is suggested.
    if let Some((key, _)) = env.iter().find(|(k, v)| {
        k.eq_ignore_ascii_case("DATABASE_URL")
            && kind_from_url(v).is_none()
            && !names_another_database(v)
            && prisma_provider.is_none()
    }) {
        let candidates: Vec<DatabaseKind> = evidence
            .keys()
            .copied()
            .filter(|k| *k != DatabaseKind::Redis)
            .collect();
        let kind = match candidates.as_slice() {
            [only] => Some(*only),
            [] => Some(DatabaseKind::Postgres),
            _ => None,
        };
        if let Some(kind) = kind {
            let list = evidence.entry(kind).or_default();
            let what = format!("`.env.example` asks for `{key}`");
            if !list.contains(&what) {
                list.insert(0, what);
            }
        }
    }

    let mut out: Vec<SuggestedService> = evidence
        .into_iter()
        .map(|(kind, evidence)| SuggestedService {
            kind,
            reason: join_evidence(&evidence),
            evidence,
        })
        .collect();
    out.sort_by(|a, b| b.evidence.len().cmp(&a.evidence.len()).then(a.kind.cmp(&b.kind)));
    out
}

fn push(evidence: &mut BTreeMap<DatabaseKind, Vec<String>>, kind: DatabaseKind, what: String) {
    let list = evidence.entry(kind).or_default();
    if !list.contains(&what) {
        list.push(what);
    }
}

/// "a, and b" / "a, b, and c", at most three.
fn join_evidence(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(3).map(String::as_str).collect();
    match shown.as_slice() {
        [] => String::new(),
        [one] => format!("{one}."),
        [a, b] => format!("{a}, and {b}."),
        [rest @ .., last] => format!("{}, and {last}.", rest.join(", ")),
    }
}

/// Whether a key looks like it holds a secret. Public, client-side keys
/// (`NEXT_PUBLIC_*`, `VITE_*`) never do, whatever their suffix.
pub fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    if ["NEXT_PUBLIC_", "VITE_", "PUBLIC_", "REACT_APP_", "NUXT_PUBLIC_", "EXPO_PUBLIC_"]
        .iter()
        .any(|p| k.starts_with(p))
        || k.contains("PUBLISHABLE")
    {
        return false;
    }
    [
        "_SECRET", "_KEY", "_TOKEN", "_PASSWORD", "_PASS", "_SALT", "_PEPPER", "_PRIVATE_KEY",
    ]
    .iter()
    .any(|s| k.ends_with(s))
        || matches!(
            k.as_str(),
            "SECRET" | "SECRET_KEY_BASE" | "APP_KEY" | "PASSWORD" | "TOKEN" | "KEY" | "SALT" | "PEPPER"
        )
}

/// Words that name what an app's own secret protects (its sessions, its
/// cookies, its tokens), never another service: `SESSION_SECRET`,
/// `JWT_SECRET`, `BETTER_AUTH_SECRET`. A name with any other word
/// (`TWILIO_API_SECRET`, `STRIPE_SECRET_KEY`, `AWS_SECRET_ACCESS_KEY`)
/// may be a credential that service issued, which a random value breaks.
const OWN_SECRET_WORDS: &[&str] = &[
    "APP",
    "APPLICATION",
    "SESSION",
    "SESSIONS",
    "COOKIE",
    "COOKIES",
    "JWT",
    "AUTH",
    "NEXTAUTH",
    "BETTER",
    "CSRF",
    "XSRF",
    "ENCRYPTION",
    "SIGNING",
    "HMAC",
    "HASH",
    "HASHING",
    "ACCESS",
    "REFRESH",
    "TOKEN",
    "ADMIN",
    "SECRET",
    "PAYLOAD",
    "DJANGO",
    "FLASK",
    "RAILS",
];

/// Whether the server can generate a value for this key: a secret the app
/// keeps for itself, never a credential some other service issues (an API
/// key, a client or webhook secret, a password the operator brings).
pub fn can_generate(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    if !looks_secret(&k) || k.contains("CLIENT_SECRET") || k.contains("WEBHOOK_SECRET") {
        return false;
    }
    if matches!(
        k.as_str(),
        "SECRET" | "SECRET_KEY" | "SECRET_KEY_BASE" | "APP_KEY" | "ENCRYPTION_KEY"
    ) {
        return true;
    }
    // A salt or pepper is always the app's own: no service issues one.
    if k.ends_with("_SALT") || k.ends_with("_PEPPER") {
        return true;
    }
    let Some(prefix) = ["_SECRET_KEY", "_SECRET", "_SIGNING_KEY", "_ENCRYPTION_KEY"]
        .iter()
        .find_map(|suffix| k.strip_suffix(suffix))
    else {
        return false;
    };
    !prefix.is_empty()
        && prefix
            .split('_')
            .all(|word| OWN_SECRET_WORDS.contains(&word))
}

/// The injected variable that fills `key` when a database of `kind` is
/// linked: itself when injected under that name, an alias otherwise.
pub fn provided_by(key: &str, kind: DatabaseKind) -> Option<&'static str> {
    let k = key.to_ascii_uppercase();
    if let Some(name) = kind.injected().iter().find(|n| **n == k) {
        return Some(name);
    }
    kind.common_aliases()
        .iter()
        .find(|(alias, _)| *alias == k)
        .map(|(_, injected)| *injected)
}

/// The aliases a link should inject for these app keys: app name → injected
/// name, only where they differ.
pub fn env_aliases<'a>(
    keys: impl IntoIterator<Item = &'a str>,
    kind: DatabaseKind,
) -> BTreeMap<String, String> {
    keys.into_iter()
        .filter_map(|key| {
            let injected = provided_by(key, kind)?;
            // Names are case-sensitive in the environment: `postgres_url`
            // is not `POSTGRES_URL`, so it gets an alias of its own.
            (key != injected).then(|| (key.to_string(), injected.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn file(path: &str, content: &str) -> RepoFile {
        RepoFile {
            path: path.into(),
            content: content.into(),
        }
    }

    #[test]
    fn env_example_and_pg_suggest_postgres_with_both_as_evidence() {
        let got = suggest_services(
            &env(&[("DATABASE_URL", ""), ("PORT", "3000")]),
            &[file(
                "package.json",
                r#"{"dependencies":{"pg":"^8","next":"15"}}"#,
            )],
            &[],
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, DatabaseKind::Postgres);
        assert_eq!(
            got[0].reason,
            "`.env.example` asks for `DATABASE_URL`, and `package.json` uses `pg`."
        );
    }

    #[test]
    fn prisma_decides_what_database_url_means() {
        let schema = "datasource db {\n  provider = \"mysql\"\n  url = env(\"DATABASE_URL\")\n}\n";
        let got = suggest_services(
            &env(&[("DATABASE_URL", "")]),
            &[
                file("package.json", r#"{"dependencies":{"@prisma/client":"5"}}"#),
                file("prisma/schema.prisma", schema),
            ],
            &[],
        );
        assert_eq!(got[0].kind, DatabaseKind::Mariadb);
        assert!(got[0].evidence.iter().any(|e| e.contains("schema.prisma")));
    }

    #[test]
    fn an_example_url_scheme_decides_too() {
        let got = suggest_services(&env(&[("DATABASE_URL", "mongodb://localhost/app")]), &[], &[]);
        assert_eq!(got[0].kind, DatabaseKind::Mongodb);
    }

    #[test]
    fn mongoose_and_redis_are_found_and_ranked_by_evidence() {
        let got = suggest_services(
            &env(&[("MONGODB_URI", ""), ("REDIS_URL", "")]),
            &[file(
                "package.json",
                r#"{"dependencies":{"mongoose":"8"},"devDependencies":{"ioredis":"5"}}"#,
            )],
            &[],
        );
        let kinds: Vec<_> = got.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![DatabaseKind::Mongodb, DatabaseKind::Redis]);
    }

    #[test]
    fn python_ruby_and_go_manifests_are_read() {
        let py = suggest_services(&[], &[file("requirements.txt", "Django>=5\npsycopg2-binary==2.9 # db\n")], &[]);
        assert_eq!(py[0].kind, DatabaseKind::Postgres);
        let poetry = suggest_services(&[], &[file("pyproject.toml", "[tool.poetry.dependencies]\npymongo = \"^4\"\n")], &[]);
        assert_eq!(poetry[0].kind, DatabaseKind::Mongodb);
        let ruby = suggest_services(&[], &[file("Gemfile", "source 'https://rubygems.org'\ngem 'rails'\ngem \"mysql2\", \"~> 0.5\"\n")], &[]);
        assert_eq!(ruby[0].kind, DatabaseKind::Mariadb);
        let go = suggest_services(&[], &[file("go.mod", "module example.com/app\n\nrequire (\n\tgithub.com/redis/go-redis/v9 v9.5.0\n)\n")], &[]);
        assert_eq!(go[0].kind, DatabaseKind::Redis);
    }

    #[test]
    fn compose_images_count() {
        let got = suggest_services(&[], &[], &["postgres:16-alpine".into(), "nginx".into()]);
        assert_eq!(got[0].kind, DatabaseKind::Postgres);
        assert_eq!(got[0].reason, "Compose runs `postgres:16-alpine`.");
        assert_eq!(kind_from_image("docker.io/library/redis:7"), Some(DatabaseKind::Redis));
        assert_eq!(kind_from_image("bitnami/valkey@sha256:abc"), Some(DatabaseKind::Redis));
        assert_eq!(kind_from_image("nginx:alpine"), None);
    }

    #[test]
    fn nothing_is_suggested_for_a_repository_that_needs_nothing() {
        let got = suggest_services(
            &env(&[("PORT", "3000"), ("NEXT_PUBLIC_SITE_URL", "")]),
            &[file("package.json", r#"{"dependencies":{"next":"15","react":"19"}}"#)],
            &[],
        );
        assert!(got.is_empty());
    }

    #[test]
    fn a_malformed_manifest_is_ignored() {
        let got = suggest_services(&[], &[file("package.json", "{ not json")], &[]);
        assert!(got.is_empty());
    }

    #[test]
    fn secrets_are_recognised_but_public_keys_never() {
        for key in [
            "SESSION_SECRET", "STRIPE_API_KEY", "GITHUB_TOKEN", "DB_PASSWORD", "SECRET_KEY_BASE", "TOKEN", "KEY",
            "PRIVATE_KEY",
        ] {
            assert!(looks_secret(key), "{key}");
        }
        for key in ["NEXT_PUBLIC_API_KEY", "VITE_MAPS_KEY", "STRIPE_PUBLISHABLE_KEY", "PORT", "NODE_ENV", "KEYCLOAK_URL"] {
            assert!(!looks_secret(key), "{key}");
        }
    }

    #[test]
    fn only_self_contained_secrets_can_be_generated() {
        assert!(can_generate("SESSION_SECRET"));
        assert!(can_generate("SECRET_KEY_BASE"));
        assert!(can_generate("APP_KEY"));
        assert!(!can_generate("STRIPE_API_KEY"));
        assert!(!can_generate("GITHUB_CLIENT_SECRET"));
        assert!(!can_generate("STRIPE_WEBHOOK_SECRET"));
        assert!(!can_generate("PORT"));
    }

    /// A secret named for another service is the one it issued: a random
    /// value in its place fails every call to it.
    #[test]
    fn a_secret_named_for_another_service_is_never_generated() {
        for key in [
            "JWT_SECRET",
            "NEXTAUTH_SECRET",
            "AUTH_SECRET",
            "BETTER_AUTH_SECRET",
            "COOKIE_SECRET",
            "APP_SECRET",
            "JWT_ACCESS_SECRET",
            "REFRESH_TOKEN_SECRET",
            "ADMIN_JWT_SECRET",
            "API_TOKEN_SALT",
            "DJANGO_SECRET_KEY",
            "SESSION_SIGNING_KEY",
            "SECRET_KEY",
        ] {
            assert!(can_generate(key), "{key}");
        }
        for key in [
            "TWILIO_API_SECRET",
            "STRIPE_SECRET",
            "STRIPE_SECRET_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SECRET",
            "SHOPIFY_API_SECRET",
            "PUSHER_APP_SECRET",
            "GOOGLE_CLIENT_SECRET",
            "API_SECRET",
            "SLACK_SIGNING_SECRET",
        ] {
            assert!(!can_generate(key), "{key}");
        }
    }

    #[test]
    fn linked_postgres_fills_database_url_through_an_alias() {
        assert_eq!(provided_by("DATABASE_URL", DatabaseKind::Postgres), Some("POSTGRES_URL"));
        assert_eq!(provided_by("POSTGRES_HOST", DatabaseKind::Postgres), Some("POSTGRES_HOST"));
        assert_eq!(provided_by("PGDATABASE", DatabaseKind::Postgres), Some("POSTGRES_DB"));
        assert_eq!(provided_by("SESSION_SECRET", DatabaseKind::Postgres), None);
        // MariaDB injects DATABASE_URL itself: no alias needed.
        assert_eq!(provided_by("DATABASE_URL", DatabaseKind::Mariadb), Some("DATABASE_URL"));

        let aliases = env_aliases(
            ["DATABASE_URL", "POSTGRES_HOST", "PGUSER", "PORT"],
            DatabaseKind::Postgres,
        );
        assert_eq!(
            aliases.into_iter().collect::<Vec<_>>(),
            vec![
                ("DATABASE_URL".to_string(), "POSTGRES_URL".to_string()),
                ("PGUSER".to_string(), "POSTGRES_USER".to_string()),
            ]
        );
        assert!(env_aliases(["DATABASE_URL"], DatabaseKind::Mariadb).is_empty());
    }

    #[test]
    fn a_lowercase_key_gets_an_alias_of_its_own() {
        // Environment names are case-sensitive: the app reads postgres_url,
        // which POSTGRES_URL does not fill.
        let aliases = env_aliases(["postgres_url", "POSTGRES_URL"], DatabaseKind::Postgres);
        assert_eq!(
            aliases.into_iter().collect::<Vec<_>>(),
            vec![("postgres_url".to_string(), "POSTGRES_URL".to_string())]
        );
    }

    #[test]
    fn a_database_temps_does_not_offer_is_not_taken_for_postgres() {
        let sqlite = "datasource db {\n  provider = \"sqlite\"\n  url = env(\"DATABASE_URL\")\n}\n";
        let got = suggest_services(
            &env(&[("DATABASE_URL", "file:./dev.db")]),
            &[file("prisma/schema.prisma", sqlite)],
            &[],
        );
        assert!(got.is_empty(), "{got:?}");
        // The example alone says so too.
        for example in ["file:./dev.db", "sqlite:///app.db", "sqlserver://db:1433"] {
            let got = suggest_services(&env(&[("DATABASE_URL", example)]), &[], &[]);
            assert!(got.is_empty(), "{example}: {got:?}");
        }
        // A bare one still means Postgres.
        let got = suggest_services(&env(&[("DATABASE_URL", "")]), &[], &[]);
        assert_eq!(got.first().map(|s| s.kind), Some(DatabaseKind::Postgres));
    }

    #[test]
    fn pyproject_dependencies_are_read_from_its_arrays_and_tables() {
        let inline = "[project]\nname = \"psycopg-app\"\ndependencies = [\"pymongo>=4\", \"flask\"]\n";
        let got = suggest_services(
            &env(&[("DATABASE_URL", "")]),
            &[file("pyproject.toml", inline)],
            &[],
        );
        assert_eq!(got.first().map(|s| s.kind), Some(DatabaseKind::Mongodb));
        assert_eq!(
            pyproject_dependencies(
                "[project]\nname = \"redis\"\nversion = \"1\"\ndependencies = [\n  \"psycopg[binary]>=3\", # db\n  \"flask\",\n]\n\n[project.optional-dependencies]\ncache = [\"redis>=5\"]\n\n[tool.poetry.dependencies]\npython = \"^3.12\"\npymongo = \"^4\"\n"
            ),
            // Extras stay with the name: `celery[redis]` means Redis.
            vec!["psycopg[binary]", "flask", "redis", "pymongo"]
        );
        // A project named like a driver is not a dependency on it.
        assert!(pyproject_dependencies("[project]\nname = \"redis\"\n").is_empty());
    }

    /// TOML's single-quoted literal strings are requirements too, with
    /// either quote inside the other's string and comments after them.
    #[test]
    fn single_quoted_pyproject_requirements_are_read() {
        let inline = "[project]\nname = \"redis\"\ndependencies = ['pymongo>=4']\n";
        assert_eq!(pyproject_dependencies(inline), vec!["pymongo"]);
        let got = suggest_services(
            &env(&[("DATABASE_URL", "")]),
            &[file("pyproject.toml", inline)],
            &[],
        );
        assert_eq!(got.first().map(|s| s.kind), Some(DatabaseKind::Mongodb));
        assert_eq!(
            pyproject_dependencies(
                "[project]\ndependencies = [\n  'psycopg[binary]>=3', # the driver\n  \"flask # web\",\n]\n"
            ),
            vec!["psycopg[binary]", "flask"]
        );
    }

    /// A package asked for with extras, or spelled another way pip accepts,
    /// is still that package.
    #[test]
    fn python_extras_and_spellings_name_the_package() {
        for (requirements, kind) in [
            ("psycopg[binary]>=3\n", DatabaseKind::Postgres),
            ("Psycopg[binary,pool] ~= 3.2\n", DatabaseKind::Postgres),
            ("mysql_connector_python==9.0\n", DatabaseKind::Mariadb),
            ("celery[redis]>=5\n", DatabaseKind::Redis),
            ("celery[ redis , msgpack ]\n", DatabaseKind::Redis),
        ] {
            let got = suggest_services(&[], &[file("requirements.txt", requirements)], &[]);
            assert_eq!(got.len(), 1, "{requirements}");
            assert_eq!(got[0].kind, kind, "{requirements}");
        }
        // Celery with another broker needs no Redis.
        let got = suggest_services(&[], &[file("requirements.txt", "celery[sqs]\n")], &[]);
        assert!(got.is_empty());
        // The same requirements in pyproject.toml's arrays.
        for pyproject in [
            "[project]\ndependencies = [\"celery[redis]>=5\"]\n",
            "[project]\ndependencies = [\n  \"celery [redis] >=5\",\n]\n",
        ] {
            let got = suggest_services(&[], &[file("pyproject.toml", pyproject)], &[]);
            assert_eq!(got.len(), 1, "{pyproject}");
            assert_eq!(got[0].kind, DatabaseKind::Redis, "{pyproject}");
        }
        let sqs = suggest_services(&[], &[file("pyproject.toml", "[project]\ndependencies = [\"celery[sqs]\"]\n")], &[]);
        assert!(sqs.is_empty());
    }

    /// CockroachDB is another engine: no managed Postgres is offered for it,
    /// even for a bare DATABASE_URL.
    #[test]
    fn a_cockroachdb_schema_is_not_offered_postgres() {
        let schema = "datasource db {\n  provider = \"cockroachdb\"\n  url = env(\"DATABASE_URL\")\n}\n";
        let got = suggest_services(
            &env(&[("DATABASE_URL", "")]),
            &[
                file("package.json", r#"{"dependencies":{"@prisma/client":"5"}}"#),
                file("prisma/schema.prisma", schema),
            ],
            &[],
        );
        assert!(got.is_empty(), "{got:?}");
    }

    /// A dependency group included by name is not a package of that name.
    #[test]
    fn a_dependency_group_reference_is_not_a_package() {
        let pyproject = "[dependency-groups]\ndev = [{ include-group = 'redis' }, \"ruff\"]\nredis = ['pytest']\n";
        assert_eq!(pyproject_dependencies(pyproject), vec!["ruff", "pytest"]);
        let got = suggest_services(&env(&[]), &[file("pyproject.toml", pyproject)], &[]);
        assert!(got.is_empty(), "{got:?}");
    }

    /// A driver after `+` in the scheme (SQLAlchemy) still names the
    /// database, and its URL is the only evidence needed.
    #[test]
    fn a_driver_qualified_url_names_its_database() {
        for (url, kind) in [
            ("postgresql+asyncpg://user@db/app", DatabaseKind::Postgres),
            ("postgresql+psycopg://db/app", DatabaseKind::Postgres),
            ("mysql+pymysql://db/app", DatabaseKind::Mariadb),
            ("mongodb+srv://cluster.example.com/app", DatabaseKind::Mongodb),
        ] {
            let got = suggest_services(&env(&[("DATABASE_URL", url)]), &[], &[]);
            assert_eq!(got.first().map(|s| s.kind), Some(kind), "{url}");
        }
        // A database Temps does not offer still suggests nothing.
        let got = suggest_services(&env(&[("DATABASE_URL", "mssql+pyodbc://db/app")]), &[], &[]);
        assert!(got.is_empty(), "{got:?}");
    }

    /// A comment after the Prisma provider is not part of its name.
    #[test]
    fn a_comment_after_the_prisma_provider_is_ignored() {
        for schema in [
            "datasource db {\n  provider = \"postgresql\" // primary database\n  url = env(\"DATABASE_URL\")\n}\n",
            "datasource db {\n  provider=\"postgresql\"\n}\n",
        ] {
            assert_eq!(prisma_provider_name(schema).as_deref(), Some("postgresql"));
            let got = suggest_services(
                &env(&[("DATABASE_URL", "")]),
                &[file("prisma/schema.prisma", schema)],
                &[],
            );
            assert_eq!(got.first().map(|s| s.kind), Some(DatabaseKind::Postgres), "{schema}");
        }
        // A key that only starts with `provider` is not the provider.
        assert_eq!(
            prisma_provider_name("datasource db {\n  providerOptions = 1\n}\n"),
            None
        );
    }

    #[test]
    fn mongo_uri_maps_onto_the_injected_url() {
        let aliases = env_aliases(["MONGODB_URI"], DatabaseKind::Mongodb);
        assert_eq!(aliases.get("MONGODB_URI").map(String::as_str), Some("MONGODB_URL"));
    }
}
