// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-instance Docker names for the managed KV and Blob services (#1446).
//!
//! The KV service runs in a container called `redis-<service name>` and the
//! Blob service in `rustfs-<service name>`, with the service name stored in
//! `external_services.name`. Both used to be the fixed `temps-kv` /
//! `temps-blob`, so a second Temps instance on the same Docker daemon could
//! not enable either service: the first instance already owned the name.
//!
//! The service name is now resolved per instance, in this order:
//!
//! 1. an already-existing service wins, so an install that enabled KV or Blob
//!    before this change keeps its running container (`temps-kv`,
//!    `temps-blob`) and nothing is orphaned by an upgrade;
//! 2. otherwise the operator's `kv.container_name` / `blob.container_name`
//!    setting, when set;
//! 3. otherwise a default derived from this instance (`temps-kv-<token>`),
//!    stable across restarts because the token comes from the instance's own
//!    encryption key, so no state needs to be stored for it.
//!
//! The name can only change while the service does not exist: renaming a
//! running service's container would orphan its data volume.

use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use temps_config::{ConfigService, ConfigServiceError};
use temps_core::problemdetails::Problem;
use temps_core::{error_builder, EncryptionService};
use temps_entities::external_services;
use thiserror::Error;
use utoipa::ToSchema;

use crate::externalsvc::{redis, rustfs};
use crate::services::ExternalServiceError;

/// Longest container name accepted: a DNS label, because other containers
/// reach the service by its container name through Docker's DNS.
pub const MAX_CONTAINER_NAME_LEN: usize = 63;

/// Domain separator for the per-instance token, so the derived key is
/// independent from every other use of the instance's encryption key.
const INSTANCE_TOKEN_DOMAIN: &str = "temps:managed-service-name:v1";

/// Which managed service a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManagedServiceKind {
    /// The KV store, a Redis container.
    Kv,
    /// The Blob store, a RustFS container.
    Blob,
}

impl ManagedServiceKind {
    /// Human name used in messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Kv => "KV",
            Self::Blob => "Blob",
        }
    }

    /// The service name installs created before names were per-instance use.
    pub fn legacy_service_name(self) -> &'static str {
        match self {
            Self::Kv => "temps-kv",
            Self::Blob => "temps-blob",
        }
    }

    /// `external_services.service_type` values a row of this kind can carry.
    /// A name resolves only to a row of the right type, so the KV store's
    /// Redis row is never mistaken for the Blob store's service (or the
    /// reverse) when both were given the same service name.
    pub fn service_types(self) -> &'static [&'static str] {
        match self {
            Self::Kv => &["redis", "kv"],
            Self::Blob => &["blob", "s3", "rustfs"],
        }
    }

    /// Whether a service row with this `service_type` is this kind's.
    pub fn owns(self, service_type: &str) -> bool {
        self.service_types().contains(&service_type)
    }

    /// The other managed kind.
    fn other(self) -> Self {
        match self {
            Self::Kv => Self::Blob,
            Self::Blob => Self::Kv,
        }
    }

    /// The setting that holds the operator's chosen container name.
    pub fn setting_key(self) -> &'static str {
        match self {
            Self::Kv => "kv.container_name",
            Self::Blob => "blob.container_name",
        }
    }

    /// The API path that edits the setting.
    pub fn settings_path(self) -> &'static str {
        match self {
            Self::Kv => "/kv/settings",
            Self::Blob => "/blob/settings",
        }
    }

    /// Docker container name the engine uses for a service called
    /// `service_name`. Asks the engine rather than re-deriving the prefix.
    pub fn container_name_for(self, service_name: &str) -> String {
        match self {
            Self::Kv => redis::redis_container_name(service_name),
            Self::Blob => rustfs::rustfs_container_name(service_name),
        }
    }

    /// The prefix every container of this kind carries.
    pub fn container_prefix(self) -> String {
        self.container_name_for("")
    }

    /// The service name that yields `container_name`, when it has this
    /// kind's prefix and something after it.
    fn service_name_from_container(self, container_name: &str) -> Option<String> {
        container_name
            .strip_prefix(self.container_prefix().as_str())
            .filter(|rest| !rest.is_empty())
            .map(str::to_string)
    }
}

/// A submitted container name Docker, or this service, cannot use.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid {setting} {name:?}: {reason}")]
pub struct InvalidManagedContainerName {
    pub setting: &'static str,
    pub name: String,
    pub reason: String,
}

/// Check a submitted container name. Returns the service name it implies, or
/// `None` for an empty submission, which means "use the per-instance default".
///
/// The accepted alphabet is deliberately narrower than Docker's: no `.`,
/// because the service name also becomes part of volume names and DNS names.
pub fn parse_container_name(
    kind: ManagedServiceKind,
    submitted: &str,
) -> Result<Option<String>, InvalidManagedContainerName> {
    let name = submitted.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let invalid = |reason: String| InvalidManagedContainerName {
        setting: kind.setting_key(),
        name: name.to_string(),
        reason,
    };
    if name.len() > MAX_CONTAINER_NAME_LEN {
        return Err(invalid(format!(
            "use at most {MAX_CONTAINER_NAME_LEN} characters (it is resolved through Docker's DNS)"
        )));
    }
    let prefix = kind.container_prefix();
    let Some(service_name) = kind.service_name_from_container(name) else {
        return Err(invalid(format!(
            "the {} container name must start with '{prefix}' followed by at least one character",
            kind.label()
        )));
    };
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    if !name.chars().all(allowed) {
        return Err(invalid(
            "use only letters, digits, '-' and '_'. Docker also allows '.', but the name is reused in volume and DNS names"
                .to_string(),
        ));
    }
    Ok(Some(service_name))
}

/// The per-instance token baked into a derived default: 8 hex characters from
/// a key derived from the instance's encryption key. Stable for the life of
/// the instance and different on every other instance, so two instances on one
/// Docker daemon get different default names without storing anything.
pub fn instance_token(encryption_service: &EncryptionService) -> String {
    let key = encryption_service.derive_subkey(INSTANCE_TOKEN_DOMAIN);
    key.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where the resolved service name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedNameSource {
    /// A service with that name already exists on this instance.
    ExistingService,
    /// The operator's setting.
    Setting,
    /// The per-instance default.
    InstanceDefault,
}

/// What a managed service is called on this instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedManagedName {
    pub kind: ManagedServiceKind,
    /// Value of `external_services.name`, and the engine's instance name.
    pub service_name: String,
    /// Docker container the engine runs the service in.
    pub container_name: String,
    /// The service name an empty setting resolves to.
    pub default_service_name: String,
    /// The container an empty setting resolves to.
    pub default_container_name: String,
    /// The operator saved a container name.
    pub custom: bool,
    /// A service with `service_name` exists, so the name cannot change.
    pub in_use: bool,
    pub source: ManagedNameSource,
}

/// Pick the service name from what is known. Pure, so the precedence is
/// testable without a database.
///
/// `existing` holds the names among the candidates that already exist as
/// services. An existing service always wins, in the order legacy, setting,
/// default: it owns data that a different name would orphan.
pub fn choose_service_name(
    kind: ManagedServiceKind,
    configured: Option<&str>,
    derived: &str,
    existing: &[String],
) -> ResolvedManagedName {
    let legacy = kind.legacy_service_name();
    let exists = |name: &str| existing.iter().any(|candidate| candidate == name);

    let (service_name, source) = if exists(legacy) {
        (legacy.to_string(), ManagedNameSource::ExistingService)
    } else if let Some(name) = configured.filter(|name| exists(name)) {
        (name.to_string(), ManagedNameSource::ExistingService)
    } else if exists(derived) {
        (derived.to_string(), ManagedNameSource::ExistingService)
    } else if let Some(name) = configured {
        (name.to_string(), ManagedNameSource::Setting)
    } else {
        (derived.to_string(), ManagedNameSource::InstanceDefault)
    };

    let default_service_name = if exists(legacy) {
        legacy.to_string()
    } else {
        derived.to_string()
    };
    ResolvedManagedName {
        kind,
        container_name: kind.container_name_for(&service_name),
        default_container_name: kind.container_name_for(&default_service_name),
        in_use: exists(&service_name),
        custom: configured.is_some(),
        default_service_name,
        service_name,
        source,
    }
}

/// Why resolving or changing a managed service name failed.
#[derive(Debug, Error)]
pub enum ManagedNameError {
    #[error("{0}")]
    Invalid(#[from] InvalidManagedContainerName),

    #[error(
        "Cannot change the {} container name to '{requested}': the {} service '{service_name}' already exists and runs in container '{current_container}'. Renaming it would orphan its data. Disable and delete the service first, then change the name",
        .kind.label(), .kind.label()
    )]
    ServiceExists {
        kind: ManagedServiceKind,
        service_name: String,
        current_container: String,
        requested: String,
    },

    #[error(
        "Cannot use '{requested}' as the {} container name: a service named '{service_name}' already exists on this instance and is not the managed {} service. Pick a different name, or delete that service first",
        .kind.label(), .kind.label()
    )]
    ServiceNameTaken {
        kind: ManagedServiceKind,
        service_name: String,
        requested: String,
    },

    #[error("Failed to read the settings while resolving the {} container name: {source}", .kind.label())]
    SettingsRead {
        kind: ManagedServiceKind,
        source: ConfigServiceError,
    },

    #[error("Failed to save {setting}: {source}")]
    SettingsWrite {
        setting: &'static str,
        source: ConfigServiceError,
    },

    #[error("Failed to look up the {} service while resolving its container name: {source}", .kind.label())]
    Database {
        kind: ManagedServiceKind,
        source: sea_orm::DbErr,
    },
}

impl From<ManagedNameError> for Problem {
    fn from(error: ManagedNameError) -> Self {
        match &error {
            ManagedNameError::Invalid(invalid) => error_builder::bad_request()
                .title("Invalid container name")
                .detail(error.to_string())
                .value("setting", invalid.setting)
                .build(),
            ManagedNameError::ServiceExists {
                service_name,
                current_container,
                ..
            } => error_builder::conflict()
                .title("Service already exists")
                .detail(error.to_string())
                .value("service_name", service_name)
                .value("container", current_container)
                .build(),
            ManagedNameError::ServiceNameTaken { service_name, .. } => error_builder::conflict()
                .title("Service name already in use")
                .detail(error.to_string())
                .value("service_name", service_name)
                .build(),
            ManagedNameError::SettingsRead { .. }
            | ManagedNameError::SettingsWrite { .. }
            | ManagedNameError::Database { .. } => error_builder::internal_server_error()
                .title("Failed to resolve the container name")
                .detail(error.to_string())
                .build(),
        }
    }
}

/// Decide whether a submitted name may be saved. `Ok(None)` means nothing
/// changes; `Ok(Some(name))` is the container name to store (empty clears it).
/// Pure, so the "never rename an existing service" rule is testable without a
/// database.
fn check_rename(
    kind: ManagedServiceKind,
    current: &ResolvedManagedName,
    requested: Option<&str>,
) -> Result<Option<String>, ManagedNameError> {
    let requested_container = match requested {
        Some(service) => kind.container_name_for(service),
        None => current.default_container_name.clone(),
    };
    if current.in_use {
        if requested_container == current.container_name {
            return Ok(None);
        }
        return Err(ManagedNameError::ServiceExists {
            kind,
            service_name: current.service_name.clone(),
            current_container: current.container_name.clone(),
            requested: requested_container,
        });
    }
    Ok(Some(
        requested
            .map(|service| kind.container_name_for(service))
            .unwrap_or_default(),
    ))
}

/// Look up the managed service for `kind` by name, treating a row of another
/// service type as not found.
///
/// KV and Blob can be saved under the same service name, and a user's own
/// service can carry it too. A lookup by name alone would then hand one kind
/// another's row, and stopping or reconfiguring it would act on the wrong
/// service.
pub async fn get_managed_service(
    manager: &crate::services::ExternalServiceManager,
    kind: ManagedServiceKind,
    name: &str,
) -> Result<external_services::Model, ExternalServiceError> {
    let row = manager.get_service_by_name(name).await?;
    if kind.owns(&row.service_type) {
        Ok(row)
    } else {
        Err(ExternalServiceError::ServiceNotFoundByName {
            name: name.to_string(),
        })
    }
}

/// Refuse a service name that is already held by a different service row.
///
/// The managed service is looked up by name, so saving a name that belongs to
/// an unrelated service (a user's own Redis called `cache`, or the other
/// managed kind's service) would make enabling or disabling it act on that
/// service. Pure, so the rule is testable without a database.
fn check_name_free(
    kind: ManagedServiceKind,
    current: &ResolvedManagedName,
    requested: Option<&str>,
    row_exists: bool,
) -> Result<(), ManagedNameError> {
    match requested {
        Some(service) if row_exists && service != current.service_name => {
            Err(ManagedNameError::ServiceNameTaken {
                kind,
                service_name: service.to_string(),
                requested: kind.container_name_for(service),
            })
        }
        Some(_) | None => Ok(()),
    }
}

/// Resolves and edits the container names of the managed KV and Blob
/// services. Cheap to construct; each plugin owns its own.
#[derive(Clone)]
pub struct ManagedServiceNames {
    db: Arc<DatabaseConnection>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
    /// Held while a name is saved and while the service is enabled, so a
    /// rename can never interleave with a service being created under the
    /// name it just resolved.
    naming_lock: Arc<tokio::sync::Mutex<()>>,
}

impl ManagedServiceNames {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            config_service,
            encryption_service,
            naming_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Serialize with name changes. Hold the guard across resolving the name
    /// and creating the service in an enable request; saving a new name waits
    /// for it and then sees the service as existing.
    pub async fn lock_naming(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.naming_lock.clone().lock_owned().await
    }

    fn derived_service_name(&self, kind: ManagedServiceKind) -> String {
        format!(
            "{}-{}",
            kind.legacy_service_name(),
            instance_token(&self.encryption_service)
        )
    }

    /// The saved container name for `kind` as a service name, ignoring one
    /// that no longer parses (it would be refused on save, so this only
    /// happens if the settings row was edited by hand).
    async fn configured_service_name(
        &self,
        kind: ManagedServiceKind,
    ) -> Result<Option<String>, ManagedNameError> {
        let settings = self
            .config_service
            .get_settings()
            .await
            .map_err(|source| ManagedNameError::SettingsRead { kind, source })?;
        let saved = match kind {
            ManagedServiceKind::Kv => settings.kv.container_name,
            ManagedServiceKind::Blob => settings.blob.container_name,
        };
        Ok(parse_container_name(kind, &saved).unwrap_or(None))
    }

    /// What `kind` is called on this instance right now.
    pub async fn resolve(
        &self,
        kind: ManagedServiceKind,
    ) -> Result<ResolvedManagedName, ManagedNameError> {
        let configured = self.configured_service_name(kind).await?;
        let derived = self.derived_service_name(kind);

        let mut candidates = vec![kind.legacy_service_name().to_string(), derived.clone()];
        candidates.extend(configured.clone());
        let existing = external_services::Entity::find()
            .filter(external_services::Column::Name.is_in(candidates))
            .filter(external_services::Column::ServiceType.is_in(kind.service_types().to_vec()))
            .all(self.db.as_ref())
            .await
            .map_err(|source| ManagedNameError::Database { kind, source })?
            .into_iter()
            .map(|service| service.name)
            .collect::<Vec<_>>();

        Ok(choose_service_name(
            kind,
            configured.as_deref(),
            &derived,
            &existing,
        ))
    }

    /// Save the container name for `kind`; an empty `submitted` clears it back
    /// to the per-instance default. Returns the name as it was and as it
    /// resolves afterwards.
    ///
    /// `apply_to_engine` runs with the resolved name while the naming lock is
    /// still held, so the plugin's engine and the saved setting change as one
    /// step with respect to an enable.
    ///
    /// Refused while the service exists unless the submission resolves to the
    /// name it already has.
    pub async fn set_container_name(
        &self,
        kind: ManagedServiceKind,
        submitted: &str,
        apply_to_engine: impl FnOnce(&ResolvedManagedName),
    ) -> Result<ContainerNameChange, ManagedNameError> {
        let requested = parse_container_name(kind, submitted)?;
        let _naming = self.lock_naming().await;
        let current = self.resolve(kind).await?;

        let Some(saved) = check_rename(kind, &current, requested.as_deref())? else {
            apply_to_engine(&current);
            return Ok(ContainerNameChange {
                previous: current.clone(),
                saved: current,
            });
        };
        // Taken by any service row (a user's own service, or the other managed
        // kind's), or by the name the other managed kind already resolves to:
        // two kinds sharing one service name would adopt each other's row.
        let row_exists = match requested.as_deref() {
            Some(service) => {
                let held_by_row = external_services::Entity::find()
                    .filter(external_services::Column::Name.eq(service))
                    .one(self.db.as_ref())
                    .await
                    .map_err(|source| ManagedNameError::Database { kind, source })?
                    .is_some();
                held_by_row || self.resolve(kind.other()).await?.service_name == service
            }
            None => false,
        };
        check_name_free(kind, &current, requested.as_deref(), row_exists)?;
        let setting = kind.setting_key();
        match kind {
            ManagedServiceKind::Kv => {
                self.config_service
                    .update_kv_settings(|kv| kv.container_name = saved.clone())
                    .await
                    .map_err(|source| ManagedNameError::SettingsWrite { setting, source })?;
            }
            ManagedServiceKind::Blob => {
                self.config_service
                    .update_blob_settings(|blob| blob.container_name = saved.clone())
                    .await
                    .map_err(|source| ManagedNameError::SettingsWrite { setting, source })?;
            }
        }
        let resolved = self.resolve(kind).await?;
        apply_to_engine(&resolved);
        Ok(ContainerNameChange {
            previous: current,
            saved: resolved,
        })
    }
}

/// A saved container name: what it was, and what it resolves to now.
#[derive(Debug, Clone)]
pub struct ContainerNameChange {
    pub previous: ResolvedManagedName,
    pub saved: ResolvedManagedName,
}

/// Response of the container-name settings endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagedContainerNameSettings {
    /// Docker container the service runs in on this instance.
    #[schema(example = "redis-temps-kv-3fa91c07")]
    pub container_name: String,
    /// The container an empty setting resolves to: the pre-existing fixed
    /// name when this install already enabled the service, otherwise a name
    /// derived from this instance.
    #[schema(example = "redis-temps-kv-3fa91c07")]
    pub default_container_name: String,
    /// Whether an operator-chosen name is saved.
    pub custom: bool,
    /// Whether the service already exists. The name cannot change then:
    /// delete the service first.
    pub in_use: bool,
    /// Every container name for this service must start with this.
    #[schema(example = "redis-")]
    pub required_prefix: String,
}

impl From<&ResolvedManagedName> for ManagedContainerNameSettings {
    fn from(resolved: &ResolvedManagedName) -> Self {
        Self {
            container_name: resolved.container_name.clone(),
            default_container_name: resolved.default_container_name.clone(),
            custom: resolved.custom,
            in_use: resolved.in_use,
            required_prefix: resolved.kind.container_prefix(),
        }
    }
}

/// Request body of the container-name settings endpoints.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateManagedContainerName {
    /// New container name. Empty resets to the per-instance default. Must
    /// start with the service's prefix (`redis-` for KV, `rustfs-` for Blob).
    #[schema(example = "redis-temps-kv-staging")]
    pub container_name: String,
}

/// Turn a failure while creating the KV or Blob service into a response.
///
/// A Docker name already taken by something that is not this instance's
/// service is a conflict the operator resolves with the container-name
/// setting, not a server fault.
pub fn enable_failure_problem(kind: ManagedServiceKind, error: &ExternalServiceError) -> Problem {
    let title = format!("Failed to Enable {} Service", kind.label());
    match error {
        ExternalServiceError::ServiceResourceConflict { name, resource } => {
            error_builder::conflict()
                .title(format!("{} container name already in use", kind.label()))
                .detail(format!(
                    "Cannot create the {label} service '{name}': the Docker resource '{resource}' already exists on this host and does not belong to this instance. Another Temps instance sharing this Docker daemon, or a leftover container or volume, owns it. Set a unique {setting} (it must start with '{prefix}'), for example via PATCH {path}, then enable the service again.",
                    label = kind.label(),
                    setting = kind.setting_key(),
                    prefix = kind.container_prefix(),
                    path = kind.settings_path(),
                ))
                .value("resource", resource)
                .value("service_name", name)
                .value("setting", kind.setting_key())
                .build()
        }
        ExternalServiceError::ServiceNameConflict { .. }
        | ExternalServiceError::ServiceNameRestoreConflict { .. }
        | ExternalServiceError::ServiceContainerRestoreConflict { .. }
        | ExternalServiceError::ServiceContainerConflict { .. }
        | ExternalServiceError::AmbiguousServiceName { .. } => error_builder::conflict()
            .title(title)
            .detail(error.to_string())
            .build(),
        // Every remaining variant is a failure to create the service that the
        // operator cannot fix through the container-name setting; they are
        // listed so a new `ExternalServiceError` has to be given a response
        // decision here.
        ExternalServiceError::ServiceNotFound { .. }
        | ExternalServiceError::ServiceNotFoundByName { .. }
        | ExternalServiceError::ServiceNotFoundBySlug { .. }
        | ExternalServiceError::InitializationFailed { .. }
        | ExternalServiceError::UpgradeRejected { .. }
        | ExternalServiceError::EncryptionFailed { .. }
        | ExternalServiceError::DecryptionFailed { .. }
        | ExternalServiceError::InvalidServiceType { .. }
        | ExternalServiceError::ServiceNotLinkedToProject { .. }
        | ExternalServiceError::ServiceClaimDenied { .. }
        | ExternalServiceError::InvalidDatabaseProvisioning { .. }
        | ExternalServiceError::ProjectNotFound { .. }
        | ExternalServiceError::EnvironmentNotFound { .. }
        | ExternalServiceError::DatabaseError { .. }
        | ExternalServiceError::ArchiveSourceDesynced { .. }
        | ExternalServiceError::ParameterValidationFailed { .. }
        | ExternalServiceError::DataSafetyRefusal { .. }
        | ExternalServiceError::StartFailed { .. }
        | ExternalServiceError::UpgradeInProgress { .. }
        | ExternalServiceError::StopFailed { .. }
        | ExternalServiceError::ClusterMemberProvisioning { .. }
        | ExternalServiceError::ClusterNotSettled { .. }
        | ExternalServiceError::ClusterBusy { .. }
        | ExternalServiceError::LockTimeout { .. }
        | ExternalServiceError::DeletionFailed { .. }
        | ExternalServiceError::ServiceHasLinkedProjects { .. }
        | ExternalServiceError::EnvironmentVariableNotFound { .. }
        | ExternalServiceError::ParameterNotFound { .. }
        | ExternalServiceError::ParameterNotSensitive { .. }
        | ExternalServiceError::EncryptedVariableAccessDenied { .. }
        | ExternalServiceError::DockerError { .. }
        | ExternalServiceError::DuplicateServiceType { .. }
        | ExternalServiceError::InternalError { .. }
        | ExternalServiceError::DockerUnavailable { .. }
        | ExternalServiceError::LocalWorkloadsDisabled { .. }
        | ExternalServiceError::ControlPlaneAddressRequired { .. }
        | ExternalServiceError::ControlPlaneMemberUnreachable { .. }
        | ExternalServiceError::ClusterPortsUnavailable { .. }
        | ExternalServiceError::ClusterMemberLimitExceeded { .. } => error_builder::internal_server_error()
            .title(title)
            .detail(format!(
                "Could not create the {} service: {error}",
                kind.label()
            ))
            .build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    const KV: ManagedServiceKind = ManagedServiceKind::Kv;
    const BLOB: ManagedServiceKind = ManagedServiceKind::Blob;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn engine_prefixes_are_the_ones_the_validator_requires() {
        assert_eq!(KV.container_prefix(), "redis-");
        assert_eq!(BLOB.container_prefix(), "rustfs-");
        // The legacy defaults map to the containers existing installs run.
        assert_eq!(
            KV.container_name_for(KV.legacy_service_name()),
            "redis-temps-kv"
        );
        assert_eq!(
            BLOB.container_name_for(BLOB.legacy_service_name()),
            "rustfs-temps-blob"
        );
    }

    #[test]
    fn empty_submission_means_the_default() {
        assert_eq!(parse_container_name(KV, ""), Ok(None));
        assert_eq!(parse_container_name(KV, "   "), Ok(None));
    }

    #[test]
    fn valid_names_yield_their_service_name() {
        assert_eq!(
            parse_container_name(KV, "redis-temps-kv-staging"),
            Ok(Some("temps-kv-staging".to_string()))
        );
        assert_eq!(
            parse_container_name(BLOB, "  rustfs-files_2  "),
            Ok(Some("files_2".to_string()))
        );
        assert!(parse_container_name(KV, &format!("redis-{}", "a".repeat(57))).is_ok());
    }

    #[test]
    fn invalid_names_are_refused_with_the_setting_and_the_reason() {
        let cases = [
            ("temps-kv", "must start with 'redis-'"),
            ("redis-", "must start with 'redis-'"),
            ("rustfs-x", "must start with 'redis-'"),
            ("redis-has space", "letters, digits"),
            ("redis-dot.name", "letters, digits"),
            ("redis-sl/ash", "letters, digits"),
            ("redis-ünï", "letters, digits"),
        ];
        for (name, expected) in cases {
            let error = parse_container_name(KV, name).expect_err(name);
            assert_eq!(error.setting, "kv.container_name", "{name}");
            assert!(error.reason.contains(expected), "{name}: {}", error.reason);
            assert!(error.to_string().contains(name.trim()), "{name}");
        }
        let too_long = format!("redis-{}", "a".repeat(58));
        let error = parse_container_name(KV, &too_long).unwrap_err();
        assert!(error.reason.contains("at most 63"), "{}", error.reason);
        assert_eq!(
            parse_container_name(BLOB, "redis-x").unwrap_err().setting,
            "blob.container_name"
        );
    }

    #[test]
    fn a_fresh_install_gets_the_derived_default() {
        let resolved = choose_service_name(KV, None, "temps-kv-3fa91c07", &[]);

        assert_eq!(resolved.service_name, "temps-kv-3fa91c07");
        assert_eq!(resolved.container_name, "redis-temps-kv-3fa91c07");
        assert_eq!(resolved.default_container_name, "redis-temps-kv-3fa91c07");
        assert_eq!(resolved.source, ManagedNameSource::InstanceDefault);
        assert!(!resolved.in_use);
        assert!(!resolved.custom);
    }

    #[test]
    fn an_install_that_already_enabled_the_service_keeps_its_container() {
        // The upgrade case: the running container is `redis-temps-kv`.
        let resolved = choose_service_name(KV, None, "temps-kv-3fa91c07", &names(&["temps-kv"]));

        assert_eq!(resolved.service_name, "temps-kv");
        assert_eq!(resolved.container_name, "redis-temps-kv");
        assert_eq!(resolved.default_container_name, "redis-temps-kv");
        assert_eq!(resolved.source, ManagedNameSource::ExistingService);
        assert!(resolved.in_use);
    }

    #[test]
    fn an_existing_legacy_service_wins_over_a_saved_name() {
        let resolved = choose_service_name(
            BLOB,
            Some("files"),
            "temps-blob-3fa91c07",
            &names(&["temps-blob"]),
        );

        assert_eq!(resolved.container_name, "rustfs-temps-blob");
        assert!(resolved.in_use);
        assert!(resolved.custom, "the saved name is still reported");
    }

    #[test]
    fn a_saved_name_is_used_while_no_service_exists() {
        let resolved = choose_service_name(KV, Some("temps-kv-staging"), "temps-kv-3fa91c07", &[]);

        assert_eq!(resolved.container_name, "redis-temps-kv-staging");
        assert_eq!(resolved.default_container_name, "redis-temps-kv-3fa91c07");
        assert_eq!(resolved.source, ManagedNameSource::Setting);
        assert!(resolved.custom);
        assert!(!resolved.in_use);
    }

    #[test]
    fn an_existing_service_under_the_saved_or_derived_name_is_found() {
        let saved =
            choose_service_name(KV, Some("custom"), "temps-kv-3fa91c07", &names(&["custom"]));
        assert_eq!(saved.container_name, "redis-custom");
        assert_eq!(saved.source, ManagedNameSource::ExistingService);
        assert!(saved.in_use);

        let derived = choose_service_name(
            KV,
            None,
            "temps-kv-3fa91c07",
            &names(&["temps-kv-3fa91c07"]),
        );
        assert_eq!(derived.container_name, "redis-temps-kv-3fa91c07");
        assert!(derived.in_use);
    }

    #[test]
    fn the_instance_token_is_stable_and_differs_between_instances() {
        let key_a = "0000000000000000000000000000000000000000000000000000000000000000";
        let key_b = "1111111111111111111111111111111111111111111111111111111111111111";
        let a = EncryptionService::new(key_a).unwrap();
        let a_again = EncryptionService::new(key_a).unwrap();
        let b = EncryptionService::new(key_b).unwrap();

        assert_eq!(instance_token(&a), instance_token(&a_again));
        assert_ne!(instance_token(&a), instance_token(&b));
        let token = instance_token(&a);
        assert_eq!(token.len(), 8);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
        // Different from the raw key: nothing of the key leaks through it.
        assert!(!key_a.contains(&token));
    }

    #[test]
    fn a_name_taken_on_the_docker_host_is_a_409_that_explains_the_fix() {
        let problem = enable_failure_problem(
            KV,
            &ExternalServiceError::ServiceResourceConflict {
                name: "temps-kv".to_string(),
                resource: "redis-temps-kv".to_string(),
            },
        );

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let detail = problem.body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("redis-temps-kv"), "{detail}");
        assert!(detail.contains("kv.container_name"), "{detail}");
        assert!(detail.contains("/kv/settings"), "{detail}");
        assert!(detail.contains("'redis-'"), "{detail}");
        assert_eq!(problem.body["resource"], "redis-temps-kv");
    }

    #[test]
    fn the_blob_conflict_names_the_blob_setting() {
        let problem = enable_failure_problem(
            BLOB,
            &ExternalServiceError::ServiceResourceConflict {
                name: "temps-blob".to_string(),
                resource: "rustfs-temps-blob".to_string(),
            },
        );

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let detail = problem.body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("blob.container_name"), "{detail}");
        assert!(detail.contains("/blob/settings"), "{detail}");
        assert!(detail.contains("rustfs-"), "{detail}");
    }

    #[test]
    fn other_name_conflicts_are_409_and_unrelated_failures_stay_500() {
        let conflict = enable_failure_problem(
            KV,
            &ExternalServiceError::ServiceNameConflict {
                name: "temps-kv".to_string(),
                existing_service_id: 4,
            },
        );
        assert_eq!(conflict.status_code, StatusCode::CONFLICT);

        let failure = enable_failure_problem(
            KV,
            &ExternalServiceError::InternalError {
                reason: "boom".to_string(),
            },
        );
        assert_eq!(failure.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failure.body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("boom"));
    }

    #[test]
    fn a_name_can_be_saved_while_no_service_exists() {
        let current = choose_service_name(KV, None, "temps-kv-3fa91c07", &[]);

        assert_eq!(
            check_rename(KV, &current, Some("temps-kv-staging")).unwrap(),
            Some("redis-temps-kv-staging".to_string())
        );
        // An empty submission clears the saved name.
        assert_eq!(
            check_rename(KV, &current, None).unwrap(),
            Some(String::new())
        );
    }

    #[test]
    fn an_existing_service_cannot_be_renamed() {
        let current = choose_service_name(KV, None, "temps-kv-3fa91c07", &names(&["temps-kv"]));

        let error = check_rename(KV, &current, Some("other")).unwrap_err();
        match error {
            ManagedNameError::ServiceExists {
                service_name,
                current_container,
                requested,
                ..
            } => {
                assert_eq!(service_name, "temps-kv");
                assert_eq!(current_container, "redis-temps-kv");
                assert_eq!(requested, "redis-other");
            }
            other => panic!("unexpected error: {other}"),
        }
        // Resetting to a default that is a different container is refused too.
        let derived_current =
            choose_service_name(KV, Some("custom"), "temps-kv-3fa91c07", &names(&["custom"]));
        assert!(check_rename(KV, &derived_current, None).is_err());
    }

    #[test]
    fn resubmitting_the_current_name_is_a_no_op_even_for_an_existing_service() {
        let current = choose_service_name(KV, None, "temps-kv-3fa91c07", &names(&["temps-kv"]));

        assert_eq!(check_rename(KV, &current, Some("temps-kv")).unwrap(), None);
        // The empty submission resolves to the same legacy container.
        assert_eq!(check_rename(KV, &current, None).unwrap(), None);
    }

    #[test]
    fn each_kind_only_adopts_rows_of_its_own_service_type() {
        // KV and Blob can be saved with the same service name; each must
        // only ever resolve to a row of its own type, so disabling Blob
        // can never stop the KV store's Redis row.
        assert!(KV.service_types().contains(&"redis"));
        assert!(!KV.service_types().contains(&"blob"));
        assert!(BLOB.service_types().contains(&"blob"));
        assert!(!BLOB.service_types().contains(&"redis"));
        for kind_type in KV.service_types() {
            assert!(
                !BLOB.service_types().contains(kind_type),
                "{kind_type} is claimed by both kinds"
            );
        }
        // Handlers go through `get_managed_service`, which keys on `owns`.
        assert!(KV.owns("redis") && !KV.owns("blob") && !KV.owns("postgres"));
        assert!(BLOB.owns("blob") && !BLOB.owns("redis"));
        assert_eq!(KV.other(), BLOB);
        assert_eq!(BLOB.other(), KV);
    }

    #[test]
    fn a_name_held_by_another_service_is_refused() {
        let current = choose_service_name(KV, None, "temps-kv-3fa91c07", &names(&[]));

        // A user's own service called `cache` exists: saving `redis-cache`
        // would make KV adopt and later stop it.
        let error = check_name_free(KV, &current, Some("cache"), true).unwrap_err();
        match &error {
            ManagedNameError::ServiceNameTaken {
                service_name,
                requested,
                ..
            } => {
                assert_eq!(service_name, "cache");
                assert_eq!(requested, "redis-cache");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);

        // Free names, resets, and re-saving the resolved name are fine.
        assert!(check_name_free(KV, &current, Some("cache"), false).is_ok());
        assert!(check_name_free(KV, &current, None, true).is_ok());
        assert!(check_name_free(KV, &current, Some(&current.service_name), true).is_ok());
    }

    #[test]
    fn name_errors_map_to_the_right_statuses() {
        let invalid = Problem::from(ManagedNameError::Invalid(
            parse_container_name(KV, "nope").unwrap_err(),
        ));
        assert_eq!(invalid.status_code, StatusCode::BAD_REQUEST);

        let exists = Problem::from(ManagedNameError::ServiceExists {
            kind: KV,
            service_name: "temps-kv".to_string(),
            current_container: "redis-temps-kv".to_string(),
            requested: "redis-other".to_string(),
        });
        assert_eq!(exists.status_code, StatusCode::CONFLICT);
        let detail = exists.body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("redis-temps-kv") && detail.contains("redis-other"));

        let database = Problem::from(ManagedNameError::Database {
            kind: BLOB,
            source: sea_orm::DbErr::Custom("down".to_string()),
        });
        assert_eq!(database.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
