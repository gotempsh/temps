// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a PostgreSQL database is told apart from a sandbox's: by its owner.

use temps_providers_core::sandbox_namespace::{
    sandbox_role_name, ExistingResource, SandboxNamespaceConflict,
};

pub const ENGINE: &str = "PostgreSQL";

/// The owner of a database, bound with its name as `$1`; no row when it does
/// not exist. Read before anything is changed, so a database that is not the
/// sandbox's is never touched.
pub const DATABASE_OWNER_QUERY: &str =
    "SELECT pg_catalog.pg_get_userbyid(datdba)::text FROM pg_catalog.pg_database \
     WHERE datname = $1";

/// What the database `database` is to the sandbox role `role`, from its owner
/// (`None` when it does not exist). Only the administrator can make a role a
/// database's owner, and the sandbox role cannot give its database away
/// (`NOCREATEDB`), so ownership is a provenance the sandbox cannot forge.
pub fn classify_database(owner: Option<&str>, role: &str) -> ExistingResource {
    match owner {
        None => ExistingResource::Missing,
        Some(owner) if owner == role => ExistingResource::Sandbox,
        Some(owner) => ExistingResource::Foreign {
            evidence: format!(
                "it is owned by the role '{owner}', not by the sandbox role '{role}'"
            ),
        },
    }
}

/// The PostgreSQL check a deployment runs before creating or reusing
/// `database` (owned by `owner`, `None` when it does not exist): a name in the
/// sandbox namespace, or a database owned by the sandbox role that name would
/// have (a sandbox's database from before the namespace, such as
/// `<project>_sandbox` for an environment slugged `sandbox`), is refused.
pub fn check_deployment_database(
    database: &str,
    owner: Option<&str>,
) -> Result<(), SandboxNamespaceConflict> {
    let role = sandbox_role_name(database);
    let evidence = match classify_database(owner, &role) {
        ExistingResource::Sandbox => Some(format!("it is owned by the sandbox role '{role}'")),
        ExistingResource::Missing | ExistingResource::Foreign { .. } => None,
    };
    temps_providers_core::sandbox_namespace::check_deployment_database(ENGINE, database, evidence)
}
