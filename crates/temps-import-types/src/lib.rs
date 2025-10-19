//! Core types and traits for the Temps import system
//!
//! This crate provides the foundational abstractions for importing workloads
//! from various sources (Docker, Coolify, Dokploy, etc.) into Temps.
//!
//! # Architecture
//!
//! - **Traits**: `ContainerImporter` defines the interface all importers must implement
//! - **Types**: Common data structures like `ImportPlan`, `ContainerSnapshot`, etc.
//! - **Errors**: Unified error handling across all importers
//!
//! # Usage
//!
//! Importer implementations (e.g., `temps-import-docker`) depend on this crate
//! and implement the `ContainerImporter` trait.

pub mod error;
pub mod importer;
pub mod plan;
pub mod snapshot;
pub mod validation;

pub use error::{ImportError, ImportResult};
pub use importer::{
    CreatedResource, ImportContext, ImportOutcome, ImportSelector, ImportServiceProvider,
    ImportSource, ImporterCapabilities, WorkloadImporter,
};
pub use plan::{
    BuildConfiguration, DeploymentStrategy, EnvironmentVariable, ImportPlan, NetworkConfiguration,
    NetworkMode, PortMapping, ResourceLimits,
};
pub use snapshot::{
    NetworkInfo, ResourceInfo, RestartPolicy, VolumeMount, VolumeType, WorkloadDescriptor,
    WorkloadId, WorkloadSnapshot, WorkloadStatus, WorkloadType,
};
pub use validation::{
    ImportValidationRule, ValidationLevel, ValidationReport, ValidationResult, ValidationStatus,
};
