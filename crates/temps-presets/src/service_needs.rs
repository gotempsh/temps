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

/// The kind a URL-shaped example value points at (`postgres://...`).
fn kind_from_url(value: &str) -> Option<DatabaseKind> {
    let scheme = value.trim().split_once("://")?.0.to_ascii_lowercase();
    match scheme.as_str() {
        "postgres" | "postgresql" => Some(DatabaseKind::Postgres),
        "mysql" | "mariadb" => Some(DatabaseKind::Mariadb),
        "mongodb" | "mongodb+srv" => Some(DatabaseKind::Mongodb),
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
        "requirements.txt" | "pyproject.toml" => match name {
            "psycopg2" | "psycopg2-binary" | "psycopg" | "asyncpg" => Some(DatabaseKind::Postgres),
            "mysqlclient" | "pymysql" | "mysql-connector-python" | "aiomysql" => {
                Some(DatabaseKind::Mariadb)
            }
            "pymongo" | "motor" | "mongoengine" => Some(DatabaseKind::Mongodb),
            "redis" | "celery[redis]" | "rq" => Some(DatabaseKind::Redis),
            _ => None,
        },
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
                l.split(['=', '<', '>', '~', '!', ';', ' '])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
        "pyproject.toml" => content
            .lines()
            .filter_map(|l| {
                // `"psycopg[binary]>=3"` in a dependency list, or
                // `psycopg = "^3"` in a Poetry table.
                let l = l.trim().trim_start_matches('"');
                let end = l
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
                    .unwrap_or(l.len());
                let name = &l[..end];
                (!name.is_empty()).then(|| name.to_string())
            })
            .collect(),
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

/// The datasource provider of a Prisma schema.
fn prisma_provider(content: &str) -> Option<DatabaseKind> {
    let mut in_datasource = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("datasource ") {
            in_datasource = true;
        } else if in_datasource && line.starts_with('}') {
            in_datasource = false;
        } else if in_datasource {
            if let Some(value) = line.strip_prefix("provider") {
                let value = value.trim().trim_start_matches('=').trim().trim_matches('"');
                return match value {
                    "postgresql" | "postgres" | "cockroachdb" => Some(DatabaseKind::Postgres),
                    "mysql" => Some(DatabaseKind::Mariadb),
                    "mongodb" => Some(DatabaseKind::Mongodb),
                    _ => None,
                };
            }
        }
    }
    None
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
    let prisma = files
        .iter()
        .find(|f| f.path.ends_with("schema.prisma"))
        .and_then(|f| prisma_provider(&f.content));

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
    if let Some((key, _)) = env.iter().find(|(k, v)| {
        k.eq_ignore_ascii_case("DATABASE_URL") && kind_from_url(v).is_none() && prisma.is_none()
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
        || matches!(k.as_str(), "SECRET" | "SECRET_KEY_BASE" | "APP_KEY" | "PASSWORD")
}

/// Whether the server can generate a value for this key: a secret that is
/// not a credential for some other service (an API key or a password the
/// operator must bring).
pub fn can_generate(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    looks_secret(&k)
        && (k.ends_with("_SECRET")
            || k.ends_with("_SALT")
            || k.ends_with("_PEPPER")
            || matches!(
                k.as_str(),
                "SECRET" | "SECRET_KEY_BASE" | "APP_KEY" | "SECRET_KEY" | "ENCRYPTION_KEY"
            ))
        && !k.contains("CLIENT_SECRET")
        && !k.contains("WEBHOOK_SECRET")
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
            (!key.eq_ignore_ascii_case(injected)).then(|| (key.to_string(), injected.to_string()))
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
        for key in ["SESSION_SECRET", "STRIPE_API_KEY", "GITHUB_TOKEN", "DB_PASSWORD", "SECRET_KEY_BASE"] {
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
    fn mongo_uri_maps_onto_the_injected_url() {
        let aliases = env_aliases(["MONGODB_URI"], DatabaseKind::Mongodb);
        assert_eq!(aliases.get("MONGODB_URI").map(String::as_str), Some("MONGODB_URL"));
    }
}
