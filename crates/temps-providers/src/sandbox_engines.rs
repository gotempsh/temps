// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The seam that lets a sandbox use a linked service without the
//! deployment's credentials (ADR-051).
//!
//! Each service type that can hand out a credential limited to a namespace of
//! its own implements [`SandboxAccessEngine`] and is listed in [`ENGINES`]:
//! a database and role for PostgreSQL, a bucket-scoped key for S3, a key
//! prefix ACL for Redis. Everything the engines share (the link check, the
//! data network, merging and redacting the variables, the audit trail) stays
//! out of them, so supporting another service type is one implementation and
//! one line in the list.
//!
//! A type with no engine is not an error to the sandbox: it is left out,
//! neither reachable nor given the deployment's credentials.

use std::collections::HashMap;

use async_trait::async_trait;
use temps_entities::external_services;

pub use temps_providers_core::sandbox_namespace::{
    check_deployment_database, is_reserved_sandbox_database_name, ExistingResource,
    SandboxNamespaceConflict, SANDBOX_DATABASE_PREFIX,
};

use crate::sandbox_access::PostgresSandboxAccess;
use crate::sandbox_mariadb::MariaDbSandboxAccess;
use crate::sandbox_mongodb::MongoDbSandboxAccess;
use crate::sandbox_redis::RedisSandboxAccess;
use crate::sandbox_s3::S3SandboxAccess;
use crate::services::{ExternalServiceError, ExternalServiceManager};

/// What an engine is given to provision a sandbox's access. The shared checks
/// (the service exists, is linked to the project) have already passed.
pub struct SandboxAccessRequest<'a> {
    pub service: &'a external_services::Model,
    /// Whose namespace on the service the sandbox gets: its project's, or
    /// its own when the link was made to the service directly. Every name an
    /// engine derives from it keeps the two kinds apart.
    pub namespace: &'a SandboxNamespace,
    /// The project the link was made through, for the audit trail.
    pub project_id: Option<i32>,
    /// The service's decrypted parameters, including its administrator
    /// credentials. Never copy them into the returned variables.
    pub parameters: &'a HashMap<String, serde_json::Value>,
}

/// A service type that can give a sandbox a credential of its own.
#[async_trait]
pub trait SandboxAccessEngine: Send + Sync {
    /// The `external_services.service_type` values this engine serves.
    fn service_types(&self) -> &'static [&'static str];

    /// The containers a sandbox must dial to use `service`, by name, or none
    /// when this engine does not cover it (a backend it does not speak, a
    /// service on another node). Put on the sandbox's private network and
    /// nothing else of the service: it must agree with what
    /// [`Self::issue`] names as the host.
    async fn containers(
        &self,
        manager: &ExternalServiceManager,
        service: &external_services::Model,
    ) -> Result<Vec<String>, ExternalServiceError>;

    /// Provision the sandbox's namespace on the service and return the
    /// variables to run code with.
    ///
    /// Must be idempotent: it runs on every connect, so a credential or
    /// namespace that was changed or dropped by hand is repaired. The
    /// variables name the container to dial and its port, consistently, and
    /// never contain the service's administrator credentials. A service the
    /// sandbox cannot reach (for example one on another node) is an
    /// [`ExternalServiceError::SandboxAccessUnsupported`], not a fallback to
    /// the shared credentials. A service this engine does not cover at all
    /// answers [`ExternalServiceError::NoSandboxAccessEngine`], which callers
    /// read as "leave it out".
    async fn issue(
        &self,
        manager: &ExternalServiceManager,
        request: SandboxAccessRequest<'_>,
    ) -> Result<HashMap<String, String>, ExternalServiceError>;
}

/// Every service type with sandbox access. Add an engine here to support one.
static ENGINES: &[&dyn SandboxAccessEngine] = &[
    &PostgresSandboxAccess,
    &S3SandboxAccess,
    &RedisSandboxAccess,
    &MariaDbSandboxAccess,
    &MongoDbSandboxAccess,
];

/// A non-empty string (or number) parameter of a service, as text.
pub(crate) fn text_parameter(
    parameters: &HashMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    parameters.get(key).and_then(|value| {
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_u64().map(|number| number.to_string()))
            .filter(|value| !value.is_empty())
    })
}

/// A password for a scoped user that survives restarts without being stored:
/// an HMAC of what it is for, keyed by the service's administrator secret. It
/// changes when that secret does, and differs per service, user and engine
/// (`tag`).
pub(crate) fn derive_password(
    admin_secret: &str,
    tag: &str,
    service_id: i32,
    name: &str,
) -> Result<String, String> {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(admin_secret.as_bytes())
        .map_err(|error| format!("could not derive the sandbox password: {error}"))?;
    mac.update(format!("{tag}:{service_id}:{name}").as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Refuse a service on another node: a sandbox can only reach the control
/// plane's Docker network, and falling back to the shared credentials is never
/// an option.
pub(crate) fn require_local(
    service: &external_services::Model,
) -> Result<(), ExternalServiceError> {
    if let crate::services::ServiceExecutionRoute::Remote(node_id) =
        crate::services::service_execution_route(service.node_id)
    {
        return Err(ExternalServiceError::SandboxAccessUnsupported {
            service_id: service.id,
            service_type: service.service_type.clone(),
            reason: format!(
                "the service runs on node {node_id}, and a sandbox can only reach services on \
                 the control plane"
            ),
        });
    }
    Ok(())
}

/// Whether `value` is a lowercase identifier safe to splice into a statement:
/// `[a-z0-9_]`, not starting with a digit, at most `max` bytes.
pub(crate) fn is_plain_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.as_bytes()[0].is_ascii_digit()
}

/// A project's sandboxes: `_sandbox_<project>`.
const PROJECT_DATABASE_PREFIX: &str = "_sandbox_";
/// A sandbox linked to a service directly: `_sandboxid_<sandbox>`. It differs
/// from [`PROJECT_DATABASE_PREFIX`] at its ninth byte (`i`, not `_`), which
/// every derived name keeps, so the two scopes can never share a database.
const SANDBOX_ID_DATABASE_PREFIX: &str = "_sandboxid_";
/// At most 59 bytes, so the role or user `sbx_<database>` (63 bytes on every
/// engine) is always the database name itself behind a fixed prefix, never a
/// truncated or hashed form that two databases could share.
const SANDBOX_DATABASE_MAX: usize = 59;
/// Hex digits of SHA-256 kept in a hashed name part: 96 bits, so nobody can
/// pick a slug that hashes like someone else's.
pub(crate) const NAME_DIGEST_LEN: usize = 24;
/// The prefix every sandbox's public id has.
const SANDBOX_PUBLIC_ID_PREFIX: &str = "sbx_";
/// The legacy name `<project>_sandbox` is only used while `sbx_` and it fit in
/// an identifier unhashed and untruncated, which keeps it one-to-one with the
/// project's slug.
const LEGACY_SUFFIX_LEN: usize = "_sandbox".len();

/// Whose sandboxes a namespace on a service belongs to. The two kinds name
/// their resources with different fixed markers (`_sandbox_` /
/// `_sandboxid_`, `sbx_` / `sbxid_`, `.sandbox` / `.id.sandbox`), so a
/// project whose slug looks like a sandbox's id can never be given that
/// sandbox's database, role, bucket or Redis user, nor the other way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNamespace {
    /// The sandboxes that reach the service through this project (its slug).
    Project(String),
    /// A sandbox linked to the service directly (its public id, `sbx_…`).
    Sandbox(String),
}

impl SandboxNamespace {
    pub fn project(slug: impl Into<String>) -> Self {
        Self::Project(slug.into())
    }

    pub fn sandbox(public_id: impl Into<String>) -> Self {
        Self::Sandbox(public_id.into())
    }

    /// The text names are made from: the part used verbatim when it is plain,
    /// and the whole value, hashed when it is not. A sandbox's id loses its
    /// fixed `sbx_` prefix (once, so `sbx_sbx_a` and `sbx_a` stay apart); an
    /// id without it is hashed whole.
    pub(crate) fn parts(&self) -> (Option<&str>, &str) {
        match self {
            Self::Project(slug) => (Some(slug.as_str()), slug.as_str()),
            Self::Sandbox(id) => (id.strip_prefix(SANDBOX_PUBLIC_ID_PREFIX), id.as_str()),
        }
    }

    /// `[a-z0-9_]` text naming this namespace within its kind, at most `max`
    /// bytes. One-to-one: a plain value (`[a-z0-9-]`, starting with a letter
    /// or digit, short enough) is itself with `-` as `_`; anything else (an
    /// accented or long slug) is `_<head>_<digest>`, which starts with `_`
    /// where a plain value cannot, and carries 96 bits of SHA-256 of the whole
    /// value. Never encodes the kind: callers put a kind marker in front.
    pub(crate) fn component(&self, max: usize) -> String {
        let (direct, whole) = self.parts();
        if let Some(direct) = direct.filter(|value| is_plain_slug(value) && value.len() <= max) {
            return direct.replace('-', "_");
        }
        use sha2::Digest;
        let digest = hex::encode(sha2::Sha256::digest(whole.as_bytes()));
        let head: String = whole
            .chars()
            .map(|character| {
                let lower = character.to_ascii_lowercase();
                if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
                    lower
                } else {
                    '_'
                }
            })
            .take(max.saturating_sub(NAME_DIGEST_LEN + 2))
            .collect();
        format!("_{head}_{}", &digest[..NAME_DIGEST_LEN])
    }

    /// The database this namespace's sandboxes share on a service:
    /// `_sandbox_<project>` or `_sandboxid_<sandbox>`, see [`Self::component`].
    pub fn database_name(&self) -> String {
        let prefix = match self {
            Self::Project(_) => PROJECT_DATABASE_PREFIX,
            Self::Sandbox(_) => SANDBOX_ID_DATABASE_PREFIX,
        };
        format!(
            "{prefix}{}",
            self.component(SANDBOX_DATABASE_MAX - prefix.len())
        )
    }

    /// The project slug whose pre-namespace database (`<project>_sandbox`)
    /// may still be adopted, when it proves to be a sandbox's. Only a
    /// project's sandboxes had one, and only for a slug that names it
    /// one-to-one: lowercase ASCII starting with a letter (a leading digit
    /// gets a `db_` that `db-…` slugs also produce, and other characters are
    /// all folded to `_`), short enough to be neither truncated nor have its
    /// role hashed. Any other slug always gets the reserved name.
    pub fn legacy_project_slug(&self) -> Option<&str> {
        match self {
            Self::Project(slug)
                if is_plain_slug(slug)
                    && slug.as_bytes()[0].is_ascii_lowercase()
                    && slug.len() + LEGACY_SUFFIX_LEN <= SANDBOX_DATABASE_MAX =>
            {
                Some(slug.as_str())
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for SandboxNamespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(slug) => write!(formatter, "project '{slug}'"),
            Self::Sandbox(id) => write!(formatter, "sandbox '{id}'"),
        }
    }
}

/// `[a-z0-9-]`, not empty, starting with a letter or digit.
pub(crate) fn is_plain_slug(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Which database a sandbox gets, given what exists under the legacy name
/// (`<namespace>_sandbox`) and under the reserved one (`_sandbox_<namespace>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxResourceChoice {
    /// The reserved name exists and is the sandbox's: use it as it is.
    Adopt,
    /// The legacy name is the sandbox's, made before the reserved namespace
    /// existed: keep using it as it is, so the sandbox keeps its data.
    AdoptLegacy,
    /// Nothing is in the way: create the database under the reserved name.
    Create,
    /// The reserved name is taken by something that is not a sandbox's.
    /// Never take it over.
    Refuse { evidence: String },
}

/// Decide which database a sandbox may use. Nothing that exists and is not
/// provably a sandbox's is ever adopted: a legacy-named database that belongs
/// to something else (an environment slugged `sandbox`, a project slugged
/// `<namespace>-sandbox`) is simply not considered, and the sandbox gets the
/// reserved name instead.
///
/// | legacy    | reserved  | choice         |
/// |-----------|-----------|----------------|
/// | any       | Sandbox   | Adopt          |
/// | Sandbox   | Missing   | AdoptLegacy    |
/// | Sandbox   | Foreign   | AdoptLegacy    |
/// | Missing   | Missing   | Create         |
/// | Foreign   | Missing   | Create         |
/// | not Sandbox | Foreign | Refuse         |
pub fn choose_sandbox_resource(
    legacy: &ExistingResource,
    reserved: &ExistingResource,
) -> SandboxResourceChoice {
    match (legacy, reserved) {
        (_, ExistingResource::Sandbox) => SandboxResourceChoice::Adopt,
        (ExistingResource::Sandbox, _) => SandboxResourceChoice::AdoptLegacy,
        (_, ExistingResource::Missing) => SandboxResourceChoice::Create,
        (_, ExistingResource::Foreign { evidence }) => SandboxResourceChoice::Refuse {
            evidence: evidence.clone(),
        },
    }
}

/// The engine for `service_type`, if that type can give a sandbox credentials
/// of its own.
pub fn engine_for(service_type: &str) -> Option<&'static dyn SandboxAccessEngine> {
    ENGINES
        .iter()
        .copied()
        .find(|engine| engine.service_types().contains(&service_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_type_has_an_engine() {
        for service_type in ["postgres", "s3", "rustfs", "redis", "mariadb", "mongodb"] {
            let engine = engine_for(service_type).expect(service_type);
            assert!(engine.service_types().contains(&service_type));
        }
    }

    #[test]
    fn a_type_with_no_engine_is_left_out() {
        for service_type in ["kv", "blob", "", "Postgres", "mysql"] {
            assert!(engine_for(service_type).is_none(), "{service_type}");
        }
    }

    #[test]
    fn no_service_type_is_claimed_by_two_engines() {
        let mut types: Vec<_> = ENGINES
            .iter()
            .flat_map(|engine| engine.service_types().iter().copied())
            .collect();
        let count = types.len();
        types.sort();
        types.dedup();
        assert_eq!(
            types.len(),
            count,
            "two engines claim the same service type"
        );
    }

    #[test]
    fn derived_passwords_differ_by_secret_service_user_and_engine() {
        let base = derive_password("secret", "tag", 1, "user").expect("password");
        assert_eq!(
            base,
            derive_password("secret", "tag", 1, "user").expect("password")
        );
        for other in [
            derive_password("other", "tag", 1, "user"),
            derive_password("secret", "tag", 2, "user"),
            derive_password("secret", "tag", 1, "someone"),
            derive_password("secret", "other-tag", 1, "user"),
        ] {
            assert_ne!(base, other.expect("password"));
        }
    }

    fn foreign() -> ExistingResource {
        ExistingResource::Foreign {
            evidence: "owned by 'postgres'".to_string(),
        }
    }

    #[test]
    fn the_sandbox_resource_decision_table() {
        use ExistingResource::{Missing, Sandbox};
        use SandboxResourceChoice::{Adopt, AdoptLegacy, Create, Refuse};
        let refuse = Refuse {
            evidence: "owned by 'postgres'".to_string(),
        };
        let cases = [
            // (legacy, reserved) -> choice
            (Missing, Missing, Create),
            (foreign(), Missing, Create),
            (Sandbox, Missing, AdoptLegacy),
            (Sandbox, foreign(), AdoptLegacy),
            (Missing, Sandbox, Adopt),
            (Sandbox, Sandbox, Adopt),
            (foreign(), Sandbox, Adopt),
            (Missing, foreign(), refuse.clone()),
            (foreign(), foreign(), refuse),
        ];
        for (legacy, reserved, expected) in cases {
            assert_eq!(
                choose_sandbox_resource(&legacy, &reserved),
                expected,
                "legacy {legacy:?}, reserved {reserved:?}"
            );
        }
    }

    #[test]
    fn a_legacy_database_that_is_not_a_sandboxs_is_never_chosen() {
        // The reported collision: `<project>_sandbox` is an environment's
        // database. Whatever the reserved name holds, the legacy one is not
        // adopted.
        for reserved in [
            ExistingResource::Missing,
            ExistingResource::Sandbox,
            foreign(),
        ] {
            assert_ne!(
                choose_sandbox_resource(&foreign(), &reserved),
                SandboxResourceChoice::AdoptLegacy
            );
        }
    }

    fn project(slug: &str) -> SandboxNamespace {
        SandboxNamespace::project(slug)
    }

    fn sandbox(id: &str) -> SandboxNamespace {
        SandboxNamespace::sandbox(id)
    }

    #[test]
    fn sandbox_database_names_are_reserved_plain_and_bounded() {
        assert_eq!(project("shop-api").database_name(), "_sandbox_shop_api");
        assert_eq!(
            sandbox("sbx_deadbeef01234567").database_name(),
            "_sandboxid_deadbeef01234567"
        );
        let long = "x".repeat(200);
        for namespace in [
            project("shop"),
            project("7days"),
            project(&long),
            project("a\"b'c`d;e"),
            project("ünï"),
            project(""),
            sandbox("sbx_ab12"),
            sandbox("sbx_AB12"),
            sandbox(""),
            sandbox(&long),
        ] {
            let name = namespace.database_name();
            assert!(is_reserved_sandbox_database_name(&name), "{name}");
            assert!(is_plain_identifier(&name, SANDBOX_DATABASE_MAX), "{name}");
            // So the role/user `sbx_<database>` fits unhashed everywhere.
            assert!(is_plain_identifier(&format!("sbx_{name}"), 63), "{name}");
        }
        assert!(!is_reserved_sandbox_database_name("shop_sandbox"));
        assert!(!is_reserved_sandbox_database_name("sandbox_shop"));
    }

    #[test]
    fn a_project_slugged_like_a_sandbox_id_never_gets_that_sandboxs_database() {
        // `sbx_ab12` used to be `sab12`, a slug any project could take.
        let ids = ["sbx_ab12", "sbx_deadbeef01234567", "sbx_AB12", "ab12", ""];
        let slugs = [
            "ab12",
            "sab12",
            "sbx-ab12",
            "id-ab12",
            "deadbeef01234567",
            "id-deadbeef01234567",
            "sandboxid-ab12",
            "café",
            "",
        ];
        for id in ids {
            let sandbox_db = sandbox(id).database_name();
            assert!(
                sandbox_db.starts_with(SANDBOX_ID_DATABASE_PREFIX),
                "{sandbox_db}"
            );
            for slug in slugs {
                let project_db = project(slug).database_name();
                assert!(
                    project_db.starts_with(PROJECT_DATABASE_PREFIX),
                    "{project_db}"
                );
                assert!(
                    !project_db.starts_with(SANDBOX_ID_DATABASE_PREFIX),
                    "{project_db}"
                );
                assert_ne!(project_db, sandbox_db, "{slug} / {id}");
                // The role/user is `sbx_<database>`, so it differs too.
                assert_ne!(format!("sbx_{project_db}"), format!("sbx_{sandbox_db}"));
            }
        }
    }

    #[test]
    fn names_stay_one_to_one_for_accented_odd_and_long_values() {
        // `slugify` keeps any Unicode letter: folding `é` and `è` to `_`
        // would give these two projects one database.
        assert_ne!(
            project("café").database_name(),
            project("cafè").database_name()
        );
        let values = [
            "café".to_string(),
            "cafè".to_string(),
            "caf".to_string(),
            "caf-".to_string(),
            "caf_".to_string(),
            "-caf".to_string(),
            "ünï".to_string(),
            "a-b".to_string(),
            "a--b".to_string(),
            "a".repeat(50),
            "a".repeat(51),
            format!("{}x", "a".repeat(80)),
            format!("{}y", "a".repeat(80)),
            String::new(),
        ];
        let mut names: Vec<String> = values
            .iter()
            .map(|value| project(value).database_name())
            .chain(values.iter().map(|value| sandbox(value).database_name()))
            .chain(
                values
                    .iter()
                    .map(|value| sandbox(&format!("sbx_{value}")).database_name()),
            )
            .collect();
        // `sbx_` is stripped once, never twice.
        names.push(sandbox("sbx_sbx_ab").database_name());
        names.push(sandbox("sbx_ab").database_name());
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "two values share a name: {names:?}");
        // A plain value is never hashed, and a hashed one is told apart by
        // the `_` a plain one cannot start with.
        assert_eq!(project(&"a".repeat(50)).database_name().len(), 59);
        assert!(project(&"a".repeat(51))
            .database_name()
            .starts_with("_sandbox__aaa"));
        assert!(project("café")
            .database_name()
            .starts_with("_sandbox__caf_"));
    }

    #[test]
    fn a_deployment_is_refused_the_sandbox_namespace_and_sandbox_databases() {
        assert!(matches!(
            check_deployment_database("MariaDB", "_sandbox_shop", None),
            Err(SandboxNamespaceConflict::ReservedForSandboxes { .. })
        ));
        let error = check_deployment_database(
            "MongoDB",
            "shop_sandbox",
            Some("the sandbox user 'sbx_shop_sandbox' is defined in it".to_string()),
        )
        .expect_err("refused");
        let message = error.to_string();
        for fragment in ["MongoDB", "'shop_sandbox'", "sbx_shop_sandbox", "Rename"] {
            assert!(message.contains(fragment), "{fragment}: {message}");
        }
        check_deployment_database("MariaDB", "shop_sandbox", None).expect("not a sandbox's");
    }

    #[test]
    fn identifiers_are_plain_lowercase_or_refused() {
        assert!(is_plain_identifier("shop_sandbox", 63));
        for bad in ["", "Shop", "1shop", "shop-x", "sh op", "a`b", "a'b", "a;b"] {
            assert!(!is_plain_identifier(bad, 63), "{bad:?}");
        }
        assert!(!is_plain_identifier(&"a".repeat(64), 63));
    }
}
