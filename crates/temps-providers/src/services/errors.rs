// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types of the external-service manager.

use temps_core::DockerUnavailable;
use thiserror::Error;

/// A short database reservation shared by every service-name writer,
/// including restore admission and final clone registration.
#[derive(Debug, thiserror::Error)]
pub enum ServiceIdentityReservationError {
    #[error("Database error while reserving service name: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Service name '{name}' is already used by service {existing_service_id}. Choose a unique name before creating or changing a service.")]
    Registered {
        name: String,
        existing_service_id: i32,
    },
    #[error("Service name '{name}' is reserved by restore run {restore_run_id}. Wait for that restore to finish or choose a unique name.")]
    Restoring { name: String, restore_run_id: i32 },
}

#[derive(Error, Debug)]
pub enum ExternalServiceError {
    #[error("Service {id} not found")]
    ServiceNotFound { id: i32 },

    #[error("Service name '{name}' is already used by service {existing_service_id}. Choose a unique name before creating or changing a service.")]
    ServiceNameConflict {
        name: String,
        existing_service_id: i32,
    },

    #[error("Service name '{name}' is reserved by restore run {restore_run_id}. Wait for that restore to finish or choose a unique name.")]
    ServiceNameRestoreConflict { name: String, restore_run_id: i32 },

    #[error("Docker container '{container}' is being created by restore run {restore_run_id}. Wait for that restore to finish before importing an unowned container.")]
    ServiceContainerRestoreConflict {
        container: String,
        restore_run_id: i32,
    },

    #[error("Service {service_id} cannot control resources for name '{name}': it is also used by service {existing_service_id}. Resolve the duplicate service records before retrying.")]
    AmbiguousServiceName {
        service_id: i32,
        name: String,
        existing_service_id: i32,
    },

    #[error("Cannot create service '{name}': Docker resource '{resource}' already exists. Choose a unique name, or use the explicit import flow to validate and adopt an existing container.")]
    ServiceResourceConflict { name: String, resource: String },

    #[error("Docker container '{container}' is already managed by service {existing_service_id}. Choose an unowned container to import; keep the existing service and its data")]
    ServiceContainerConflict {
        container: String,
        existing_service_id: i32,
    },

    #[error("Service with name '{name}' not found")]
    ServiceNotFoundByName { name: String },

    #[error("Service with slug '{slug}' not found")]
    ServiceNotFoundBySlug { slug: String },

    #[error("Failed to initialize service {id}: {reason}")]
    InitializationFailed { id: i32, reason: String },

    /// A same-version `upgrade()` call was rejected before touching the
    /// container (unsupported downgrade, or a major-version change that
    /// must go through the dedicated upgrade orchestrator instead). Client
    /// error, not a server failure -- kept distinct from
    /// `InitializationFailed` so handlers can map it to 400 without
    /// string-matching the message.
    #[error("Upgrade rejected for service {id}: {reason}")]
    UpgradeRejected { id: i32, reason: String },

    #[error("Failed to encrypt parameter '{param_name}' for service {service_id}: {reason}")]
    EncryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Failed to decrypt parameter '{param_name}' for service {service_id}: {reason}")]
    DecryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Invalid service type '{service_type}' for service {id}")]
    InvalidServiceType { id: i32, service_type: String },

    #[error("Service {service_id} is not linked to project {project_id}")]
    ServiceNotLinkedToProject { service_id: i32, project_id: i32 },

    #[error("Service {service_id} ({service_type}) has no sandbox access engine: this type cannot give a sandbox credentials of its own yet")]
    NoSandboxAccessEngine {
        service_id: i32,
        service_type: String,
    },

    #[error(
        "Service {service_id} ({service_type}) cannot give a sandbox its own credentials: {reason}"
    )]
    SandboxAccessUnsupported {
        service_id: i32,
        service_type: String,
        reason: String,
    },

    /// A database a sandbox or a deployment asked for belongs to the other
    /// side, so it was left untouched (ADR-051). A conflict, not a failure:
    /// the operator renames or removes one side.
    #[error("Service {service_id}: {conflict}")]
    SandboxNamespaceConflict {
        service_id: i32,
        conflict: crate::sandbox_engines::SandboxNamespaceConflict,
    },

    #[error("Service {service_id} is no longer available to claim")]
    ServiceClaimDenied { service_id: i32 },

    #[error(
        "Invalid database provisioning for service {service_id} in project {project_id}: {reason}"
    )]
    InvalidDatabaseProvisioning {
        service_id: i32,
        project_id: i32,
        reason: String,
    },

    #[error("Project {id} not found")]
    ProjectNotFound { id: i32 },

    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        environment_id: i32,
        project_id: i32,
    },

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    /// `repoint_continuous_archive_source` physically repoints the
    /// container's `archive_command` before persisting the new pin -- if the
    /// persist step then fails (after retrying), the live WAL destination
    /// and the recorded pin disagree, and every later mirror/restore
    /// decision keyed on the pin (`temps-cloud`'s `backup_mirror.rs`) is
    /// wrong until this is reconciled. Kept distinct from `DatabaseError` so
    /// this specific, actionable state is never mistaken for an ordinary
    /// transient failure that left nothing inconsistent behind.
    ///
    /// `message` is computed at construction time to produce an engine-accurate
    /// description. Postgres/Timescale physically repoints WAL-G's
    /// `archive_command` before persisting, so a DB failure creates a genuine
    /// live desync. MariaDB's shipper re-reads the pin every tick, so if the
    /// DB persist fails there is no live desync — archiving has not moved.
    #[error("{message}")]
    ArchiveSourceDesynced {
        service_id: i32,
        new_s3_source_id: i32,
        attempts: u32,
        reason: String,
        /// `true` when the container-side archive was physically repointed
        /// before the DB persist failed (Postgres/Timescale: WAL-G
        /// `archive_command` already rewritten). `false` for MariaDB: the pin
        /// update is the entire repoint, so nothing changed on the container.
        physical_repoint_occurred: bool,
        /// Engine-accurate error text derived from `physical_repoint_occurred`.
        message: String,
    },

    #[error("Parameter validation failed for service {service_id}: {reason}")]
    ParameterValidationFailed { service_id: i32, reason: String },

    /// A change was refused because carrying it out would leave the service
    /// running without the data it has: its container would be replaced by
    /// one on other storage, or a rename would start it on a new, empty
    /// volume (#1415). Nothing was changed.
    #[error("Refusing to change service {service_id} ('{name}'): {reason}")]
    DataSafetyRefusal {
        service_id: i32,
        name: String,
        reason: String,
    },

    #[error("Failed to start service {id}: {reason}")]
    StartFailed { id: i32, reason: String },

    #[error(
        "Service {id} has a major upgrade in progress (upgrade_id={upgrade_id}, phase={phase}); \
         refusing to start/reconcile the container until it completes or is cancelled"
    )]
    UpgradeInProgress {
        id: i32,
        upgrade_id: i32,
        phase: String,
    },

    #[error("Failed to stop service {id}: {reason}")]
    StopFailed { id: i32, reason: String },

    #[error(
        "Cannot {action} cluster service {service_id}: member '{container_name}' is still being \
         provisioned ({status}). Wait until it is running or failed, then retry"
    )]
    ClusterMemberProvisioning {
        service_id: i32,
        action: &'static str,
        container_name: String,
        status: String,
    },

    #[error("Cannot {action} cluster service {service_id}: the cluster is '{status}'. {remedy}")]
    ClusterNotSettled {
        service_id: i32,
        action: &'static str,
        status: String,
        remedy: &'static str,
    },

    #[error(
        "Cannot {action} cluster service {service_id}: another Stop or Start is already in \
         progress on it. Try again once it finishes"
    )]
    ClusterBusy {
        service_id: i32,
        action: &'static str,
    },

    /// A database lock this operation needs was still held by another
    /// transaction after `timeout_secs`. The wait is bounded so a stuck
    /// holder surfaces as this error naming the service and the operation,
    /// not as a request that never returns. Only the database write was
    /// refused: a caller that saves after Docker work (an upgrade, say) may
    /// already have changed the container, so this does not promise the
    /// service is untouched.
    #[error(
        "Timed out after {timeout_secs}s waiting for a database lock while {operation} \
         service {service_id}: another operation is holding it. The database change was \
         not applied; check the service's state, and retry once the other operation finishes"
    )]
    LockTimeout {
        service_id: i32,
        operation: &'static str,
        timeout_secs: u64,
    },

    #[error("Failed to delete service {id}: {reason}")]
    DeletionFailed { id: i32, reason: String },

    #[error("Cannot delete service {service_id}: still linked to {project_count} project(s)")]
    ServiceHasLinkedProjects {
        service_id: i32,
        project_count: usize,
    },

    #[error("Environment variable '{var_name}' not found for service {service_id}")]
    EnvironmentVariableNotFound { service_id: i32, var_name: String },

    #[error("Parameter '{param_name}' not found for service {service_id}")]
    ParameterNotFound { service_id: i32, param_name: String },

    #[error("Parameter '{param_name}' for service {service_id} is not sensitive")]
    ParameterNotSensitive { service_id: i32, param_name: String },

    #[error("Access denied for encrypted variable '{var_name}' in service {service_id}")]
    EncryptedVariableAccessDenied { service_id: i32, var_name: String },

    #[error("Docker operation failed for service {id}: {reason}")]
    DockerError { id: i32, reason: String },

    #[error("Project {project_id} already has a linked service of type '{service_type}'")]
    DuplicateServiceType {
        project_id: i32,
        service_type: String,
    },

    #[error("Internal error: {reason}")]
    InternalError { reason: String },

    /// The local Docker daemon is structurally unavailable in this process —
    /// the process was started without a socket (e.g. a containerised
    /// control plane). Use [`Self::LocalWorkloadsDisabled`] when the daemon
    /// *could* be present but the serve profile forbids using it.
    #[error(transparent)]
    DockerUnavailable(#[from] DockerUnavailable),

    /// A request tried to provision or start a container locally on a process
    /// that was started with `--profile control-plane`. Workloads run on
    /// worker nodes instead; the HTTP surface stays mounted so the console
    /// can still list remote services and explain that provisioning is
    /// unavailable here.
    #[error(
        "Managed service '{name}' cannot run on this control plane: it was started with serve \
         profile 'control-plane', which runs no local containers. Create the service on a \
         worker node (set `node_id`) — join one with `temps join` — or run the control \
         plane with `--profile full`"
    )]
    LocalWorkloadsDisabled { name: String },

    /// A cluster spans the control plane and worker nodes, but the control
    /// plane has no address its workers can reach. Members on the control
    /// plane would advertise an address nothing on a worker can connect to,
    /// so data nodes could never register with a control-plane monitor.
    #[error(
        "Cluster '{name}' has members on both the control plane and worker nodes, but the \
         control plane has no address the workers can reach: {reason}. Start the control plane \
         with `temps serve --private-address <IP>` using its address on the network the workers \
         share, or place every member on worker nodes"
    )]
    ControlPlaneAddressRequired { name: String, reason: String },

    /// A worker member was requested for a cluster whose existing
    /// control-plane member only publishes its port on loopback.
    #[error(
        "Cannot add a worker-node member to cluster '{name}': its control-plane member \
         '{container_name}' publishes port {port} only on loopback, not on the control plane's \
         address {address}, so a member on a worker could never reach it. The cluster was \
         created with every member on the control plane (or by an earlier release); recreate it \
         with the members placed where they should run"
    )]
    ControlPlaneMemberUnreachable {
        name: String,
        container_name: String,
        port: u16,
        address: String,
    },

    /// A cluster's members cannot be given host ports: either its 10-port
    /// block is full, or no free block is left in the cluster port range.
    #[error("Cannot assign host ports for cluster service {service_id}: {reason}")]
    ClusterPortsUnavailable { service_id: i32, reason: String },

    /// More members were requested than a cluster's port block can hold.
    #[error(
        "Cluster '{name}' requests {requested} members, but a cluster has at most {max} \
         (one host port each in its {max}-port block)"
    )]
    ClusterMemberLimitExceeded {
        name: String,
        requested: usize,
        max: u16,
    },
}

impl From<ServiceIdentityReservationError> for ExternalServiceError {
    fn from(error: ServiceIdentityReservationError) -> Self {
        match error {
            ServiceIdentityReservationError::Database(error) => error.into(),
            ServiceIdentityReservationError::Registered {
                name,
                existing_service_id,
            } => Self::ServiceNameConflict {
                name,
                existing_service_id,
            },
            ServiceIdentityReservationError::Restoring {
                name,
                restore_run_id,
            } => Self::ServiceNameRestoreConflict {
                name,
                restore_run_id,
            },
        }
    }
}

impl From<sea_orm::DbErr> for ExternalServiceError {
    fn from(err: sea_orm::DbErr) -> Self {
        ExternalServiceError::DatabaseError {
            reason: err.to_string(),
        }
    }
}

impl From<anyhow::Error> for ExternalServiceError {
    fn from(err: anyhow::Error) -> Self {
        ExternalServiceError::InternalError {
            reason: err.to_string(),
        }
    }
}

impl From<sea_orm::TransactionError<ExternalServiceError>> for ExternalServiceError {
    fn from(err: sea_orm::TransactionError<ExternalServiceError>) -> Self {
        match err {
            sea_orm::TransactionError::Connection(e) => ExternalServiceError::DatabaseError {
                reason: e.to_string(),
            },
            sea_orm::TransactionError::Transaction(e) => e,
        }
    }
}
