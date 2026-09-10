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
