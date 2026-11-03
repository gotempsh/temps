// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The database names only sandboxes may use, and the check every deployment
//! path of every engine runs against them.

use sha2::{Digest, Sha256};

/// The longest identifier the database engines accept.
pub const IDENTIFIER_MAX: usize = 63;
/// The name of a sandbox's role or user starts with this.
pub const ROLE_PREFIX: &str = "sbx_";

/// Every sandbox database (PostgreSQL, MariaDB, MongoDB) has a name starting
/// with this: `_sandbox_<project>` for a project's sandboxes,
/// `_sandboxid_<sandbox>` for a sandbox linked to the service directly (see
/// [`SandboxNamespace::database_name`]).
///
/// Deployments name their databases `<project>_<environment>`, `<project>` or
/// a custom name. A slug never starts with `_`, and custom names and
/// deployment provisioning both refuse this prefix (see
/// [`check_deployment_database`]), so no deployment can produce, create or be
/// handed a name in this namespace, and a sandbox can never be pointed at a
/// deployment's database by a name collision.
///
/// The names before this namespace existed (`<project>_sandbox`) are still
/// used for a project's sandbox database that provably belongs to a sandbox,
/// so existing sandboxes keep their data; see [`choose_sandbox_resource`] and
/// [`SandboxNamespace::legacy_project_slug`].
pub const SANDBOX_DATABASE_PREFIX: &str = "_sandbox";

/// Whether `database` is in the namespace only sandboxes may use.
pub fn is_reserved_sandbox_database_name(database: &str) -> bool {
    database.starts_with(SANDBOX_DATABASE_PREFIX)
}

/// What a database with a sandbox's name turned out to be, judged by a signal
/// only the service's administrator can set: the database's owner on
/// PostgreSQL, the existence of the sandbox's own user on MariaDB and MongoDB
/// (a sandbox's credential cannot create, drop or re-own any of these).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingResource {
    /// Nothing by that name exists.
    Missing,
    /// It exists and was provisioned for a sandbox.
    Sandbox,
    /// It exists and was not provisioned for a sandbox. `evidence` says how
    /// that is known, for the error message.
    Foreign { evidence: String },
}

/// A database that one side (a sandbox or a deployment) must not be given
/// because it belongs to the other.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxNamespaceConflict {
    /// The sandbox's database name is taken by a database that was not
    /// provisioned for a sandbox.
    #[error(
        "the {engine} database '{database}' already exists and was not created for a sandbox \
         ({evidence}); it belongs to something else, so it was left untouched and the sandbox \
         was not given access to it. If nothing uses it, drop or rename it, then connect the \
         sandbox again"
    )]
    NotASandboxDatabase {
        engine: &'static str,
        database: String,
        evidence: String,
    },
    /// A deployment's database name is a sandbox's database: handing it to
    /// the deployment would let code running in the sandbox read and change
    /// the deployment's data.
    #[error(
        "refusing to provision the {engine} database '{database}' for a deployment: it is a \
         sandbox's database ({evidence}), and code running in a sandbox holds credentials to \
         it. Rename the environment or project so its database name changes, or, if the \
         sandbox's data is not needed, drop that database and the sandbox's '{sandbox_user}' \
         user or role first"
    )]
    SandboxOwnedDatabase {
        engine: &'static str,
        database: String,
        sandbox_user: String,
        evidence: String,
    },
    /// A deployment asked for a name in the sandbox namespace.
    #[error(
        "refusing to provision the {engine} database '{database}' for a deployment: names \
         starting with '{prefix}' are reserved for sandbox databases. Choose another custom \
         database name"
    )]
    ReservedForSandboxes {
        engine: &'static str,
        database: String,
        prefix: &'static str,
    },
}

/// The check every deployment path runs before creating or reusing the
/// database `database`. `sandbox_evidence` is set when the database exists and
/// carries a sandbox's provenance (see [`ExistingResource`]): the deployment
/// must not be given it.
pub fn check_deployment_database(
    engine: &'static str,
    database: &str,
    sandbox_evidence: Option<String>,
) -> Result<(), SandboxNamespaceConflict> {
    if is_reserved_sandbox_database_name(database) {
        return Err(SandboxNamespaceConflict::ReservedForSandboxes {
            engine,
            database: database.to_string(),
            prefix: SANDBOX_DATABASE_PREFIX,
        });
    }
    match sandbox_evidence {
        Some(evidence) => Err(SandboxNamespaceConflict::SandboxOwnedDatabase {
            engine,
            database: database.to_string(),
            sandbox_user: sandbox_role_name(database),
            evidence,
        }),
        None => Ok(()),
    }
}

/// The role that owns `database`. Roles are cluster-wide like databases, so the
/// name is derived from the database name; when that would not fit in an
/// identifier it keeps a prefix and a digest of the whole name instead, so two
/// long names cannot collapse into one role.
pub fn sandbox_role_name(database: &str) -> String {
    let direct = format!("{ROLE_PREFIX}{database}");
    if direct.len() <= IDENTIFIER_MAX {
        return direct;
    }
    let digest = hex::encode(Sha256::digest(database.as_bytes()));
    let keep = IDENTIFIER_MAX - ROLE_PREFIX.len() - 1 - 12;
    format!(
        "{ROLE_PREFIX}{}_{}",
        database.chars().take(keep).collect::<String>(),
        &digest[..12]
    )
}
