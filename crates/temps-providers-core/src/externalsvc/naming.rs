// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Canonical naming for managed service instances.
//!
//! A managed service is addressed by two different names:
//!
//! - the **service name** stored in `external_services.name` (`temps-blob`),
//!   which is what the API and the console show;
//! - the **instance name** handed to the engine (`RustfsService`,
//!   `RedisService`, …), which the engine turns into a Docker container name
//!   by prefixing its driver (`rustfs-{instance}`, `redis-{instance}`).
//!
//! These must be derived in exactly one place. They were not: the
//! `ExternalServiceManager` built `Blob`/`Kv` instances with an extra
//! `blob-`/`kv-` prefix while the blob and kv plugins built theirs from the
//! bare service name. One logical service therefore owned two containers —
//! `rustfs-blob-temps-blob` (created on enable, never read) and
//! `rustfs-temps-blob` (created on every boot, holding all the data) — and
//! deleting the service removed only the first. See issue #495.
//!
//! The bare service name is canonical: it is the one the plugins created and
//! the one whose volumes hold user data.

use super::ServiceType;

/// Instance name for a managed service — the name the engine receives and
/// turns into its container name.
///
/// This is the service name verbatim for every type. The function exists so
/// there is a single named place that says so, and so the plugins and the
/// `ExternalServiceManager` provably agree instead of each spelling out a
/// derivation of their own.
pub fn managed_instance_name(service_name: &str, _service_type: ServiceType) -> String {
    service_name.to_string()
}

/// Instance names an earlier revision may have used for this built-in service.
///
/// Installs created before the naming split was fixed have a container under
/// the prefixed name. Callers use these to reach those containers — to sweep
/// them on delete, or to tell the operator they are there.
///
/// The sweep is intentionally limited to the two built-in services that the
/// plugins manage. For arbitrary user-created services, deriving
/// `blob-{service_name}` or `kv-{service_name}` can collide with a normal
/// RustFS/Redis service that already uses that name, so returning a legacy
/// alias for those names would let one service delete another service's
/// container and volumes.
pub fn legacy_managed_instance_names(service_name: &str, service_type: ServiceType) -> Vec<String> {
    match (service_name, service_type) {
        // `ExternalServiceManager::create_service_instance` prefixed these
        // two built-in services before the fix for #495.
        ("temps-blob", ServiceType::Blob) => vec!["blob-temps-blob".to_string()],
        ("temps-kv", ServiceType::Kv) => vec!["kv-temps-kv".to_string()],
        _ => Vec::new(),
    }
}

/// POSIX-safe shell escaping: wraps value in single quotes, escaping any
/// embedded single quotes. Safe for use in `sh -c` command strings.
///
/// Shared across the Postgres provider files (`postgres_upgrade`,
/// `postgres_lifecycle`) instead of each keeping its own copy -- the drift
/// between duplicated copies of this exact kind of string-builder is what
/// caused the healthcheck bug this crate's `postgres_healthcheck_cmd` fixes.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Builds the `pg_isready` healthcheck command pinned to the configured
/// username/database. Without `-d`, `pg_isready` (via libpq) defaults the
/// target database to the username, so any service where `database !=
/// username` would log a FATAL on every healthcheck tick.
///
/// `exec` replaces the `/bin/sh -c` wrapper Docker starts for a `CMD-SHELL`
/// healthcheck, so `pg_isready` is the process Docker kills when the probe
/// exceeds its timeout. Without it the image's `dash` forks `pg_isready` as a
/// child, the kill takes only the shell, and the orphan is adopted by PID 1
/// -- the postmaster. When it then gives up with exit code 2 ("no response"),
/// PostgreSQL 18 logs `untracked child process ... exited with exit code 2`
/// and treats it as a crashed backend: every connection is terminated and
/// the server runs crash recovery. One slow probe therefore became a full
/// connection reset. `POSTGRES_CONTAINER_INIT` is the second guard against
/// the same failure from any other exec'd command.
///
/// Probe TCP explicitly: the entrypoint's temporary initialization server only
/// listens on a Unix socket. A socket probe can mark the container healthy just
/// before that server shuts down, causing the first deployment to lose its
/// database connection. TCP becomes available when the final server starts.
///
/// Shared with `postgres_lifecycle` so both container-creation sites build
/// the exact same healthcheck command.
pub fn postgres_healthcheck_cmd(username: &str, database: &str) -> String {
    format!(
        "exec pg_isready -h 127.0.0.1 -p 5432 -U {} -d {}",
        shell_escape(username),
        shell_escape(database)
    )
}

/// Container name of a service named `name`. The single derivation shared by
/// the engine and anything that must find its container without an instance.
pub fn postgres_container_name(name: &str) -> String {
    format!("postgres-{}", name)
}

/// Container name of a service named `name`. The single derivation shared by
/// the engine and anything that must find its container without an instance.
pub fn redis_container_name(name: &str) -> String {
    format!("redis-{}", name)
}

/// Container name of a service named `name`. The single derivation shared by
/// the engine and anything that must find its container without an instance.
pub fn mongodb_container_name(name: &str) -> String {
    format!("temps-mongodb-{}", name)
}

/// `CMD-SHELL` script used as the Docker healthcheck for MongoDB containers.
///
/// **The credentials are deliberately absent from `mongosh`'s argv.** They are
/// read inside the `--eval` script from `process.env`, which mongosh exposes to
/// evaluated JavaScript. This is not a style preference:
///
/// `mongosh` is a Node CLI whose argument parser treats *any* token beginning
/// with `-` as a new flag, including in the value position of a space-separated
/// `-u`, `-p` or `--password`. Because [`generate_secure_password`] draws from a
/// charset containing `-`, roughly one in seventy-three auto-generated MongoDB
/// passwords starts with one. Such a password was quoted perfectly by the shell
/// and still reached mongosh as a clean argv token that its own parser rejected:
///
/// ```text
/// MongoshUnimplementedError: [COMMON-10001] Error parsing command line: unrecognized option: -<password>
/// ```
///
/// Every probe then failed and service creation stalled for the full 90-second
/// health-check budget in [`MongodbService::wait_for_container_health`].
///
/// Neither shape that keeps the other providers safe helps here, both verified
/// against a real `gotempsh/mongodb-walg:8.0` container:
///
/// * the glued `-p<value>` form that makes `mariadb-admin` immune is **not**
///   accepted by mongosh — it ignores the glued value, prompts `Enter password:`
///   on stdin and then fails authentication, which is a worse failure than the
///   parse error because it looks like a credential problem;
/// * `--password=<value>` does work, but protects only the password and leaves
///   `-u <username>` open to the identical parse error.
///
/// Keeping both values off argv entirely is the only form immune to *every*
/// username and password shape, including ones an operator sets by hand through
/// the API rather than ones this crate generated.
///
/// Note the probe must authenticate to be meaningful: `ping` is answerable on an
/// unauthenticated connection, so a bare `db.adminCommand({ping: 1})` reports
/// healthy regardless of the credentials. `db.auth()` throwing on rejection is
/// what makes this check fail closed.
pub const HEALTHCHECK_COMMAND: &str = concat!(
    "mongosh --norc --quiet --eval ",
    "'db.getSiblingDB(\"admin\").auth(process.env.MONGO_INITDB_ROOT_USERNAME, ",
    "process.env.MONGO_INITDB_ROOT_PASSWORD); db.adminCommand({ping: 1})'",
    " || exit 1",
);

/// Container name of a service named `name`. The single derivation shared by
/// the engine and anything that must find its container without an instance.
pub fn mariadb_container_name(name: &str) -> String {
    format!("mariadb-{}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this module exists for: the manager and the blob plugin
    /// must derive the same instance name, or `delete_service` removes a
    /// container nobody was using and leaves the one holding the data.
    #[test]
    fn blob_instance_name_matches_the_name_the_plugin_uses() {
        assert_eq!(
            managed_instance_name("temps-blob", ServiceType::Blob),
            "temps-blob",
            "the blob plugin creates `rustfs-temps-blob`; a prefixed instance \
             name would make delete_service target a different container"
        );
    }

    /// `temps-kv` persists `ServiceType::Redis`, so the `Kv` branch is not on
    /// a live path today. Pin it anyway: persisting `Kv` is the obvious
    /// cleanup, and before this fix it would have reintroduced #495 for KV.
    #[test]
    fn kv_instance_name_matches_the_name_the_plugin_uses() {
        assert_eq!(
            managed_instance_name("temps-kv", ServiceType::Kv),
            "temps-kv",
            "the kv plugin creates `redis-temps-kv`; a prefixed instance name \
             would make delete_service target a different container"
        );
    }

    #[test]
    fn unprefixed_types_are_passed_through_untouched() {
        for service_type in [
            ServiceType::Postgres,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Rustfs,
            ServiceType::Mariadb,
            ServiceType::Mongodb,
        ] {
            assert_eq!(managed_instance_name("svc", service_type), "svc");
            assert!(
                legacy_managed_instance_names("svc", service_type).is_empty(),
                "{service_type:?} was never renamed, so it has no legacy alias to sweep"
            );
        }
    }

    /// The sweep in `delete_service` reaches the pre-fix containers through
    /// these names, so they must stay exactly what the old code produced.
    #[test]
    fn legacy_names_reproduce_the_pre_fix_prefixes_for_builtin_services_only() {
        assert_eq!(
            legacy_managed_instance_names("temps-blob", ServiceType::Blob),
            vec!["blob-temps-blob".to_string()],
            "old container was rustfs-blob-temps-blob"
        );
        assert_eq!(
            legacy_managed_instance_names("temps-kv", ServiceType::Kv),
            vec!["kv-temps-kv".to_string()],
            "old container was redis-kv-temps-kv"
        );
    }

    #[test]
    fn legacy_names_are_not_derived_for_user_chosen_names() {
        assert!(
            legacy_managed_instance_names("foo", ServiceType::Blob).is_empty(),
            "deleting Blob `foo` must not sweep an unrelated RustFS/S3 service named `blob-foo`"
        );
        assert!(
            legacy_managed_instance_names("foo", ServiceType::Kv).is_empty(),
            "deleting KV `foo` must not sweep an unrelated Redis service named `kv-foo`"
        );
    }
}
