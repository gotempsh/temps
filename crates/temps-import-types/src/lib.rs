//! Core types and traits for the Temps import system
//!
//! This crate provides the foundational abstractions for importing workloads
//! from various sources (Docker, Coolify, Vercel, Railway, etc.) into Temps.
//!
//! # Architecture
//!
//! - **Traits**: [`WorkloadImporter`] defines the interface all importers must implement
//! - **Types**: Common data structures like [`ImportPlan`], [`WorkloadSnapshot`],
//!   [`ProjectSnapshot`], [`MigrationStep`], etc.
//! - **Errors**: Unified error handling across all importers
//!
//! # Safety-first design
//!
//! The import plan is designed for **transparency and safety**:
//! - Every action has a human-readable description and risk level
//! - Data implications are surfaced explicitly so users can make informed decisions
//! - Migration runs as ordered steps with per-step results
//! - Users can skip individual services, domains, or steps before execution
//!
//! # Usage
//!
//! Importer implementations (e.g., `temps-import-docker`, `temps-import-vercel`)
//! depend on this crate and implement the [`WorkloadImporter`] trait.

pub mod cost;
pub mod error;
pub mod importer;
pub mod plan;
pub mod snapshot;
pub mod validation;

// Cost analysis types
pub use cost::{
    CloudProvider, ClusterCapacity, CostAnalysis, NodeCostInfo, OverprovisioningAssessment,
    OverprovisioningVerdict, ResourceFootprint, TargetRecommendation, UsageSource,
};

// Error types
pub use error::{ImportError, ImportResult};

// Importer trait and related types
pub use importer::{
    CreatedResource, CredentialValidation, ImportContext, ImportCredentials, ImportOutcome,
    ImportSelector, ImportServiceProvider, ImportSource, ImporterCapabilities, StepResult,
    WorkloadImporter,
};

// Plan types
pub use plan::{
    BuildConfiguration, DataImplication, DataImplicationSeverity, DeploymentStrategy, DomainAction,
    DomainPlan, EnvironmentVariable, GitSourcePlan, ImportPlan, ManualAction, ManualActionTiming,
    MigrationStep, MigrationSummary, NetworkConfiguration, NetworkMode, PortMapping,
    ResourceCounts, ResourceLimits, RiskLevel, ServiceAction, ServicePlan, StepResourceType,
    UnsupportedFeature,
};

// Snapshot types
pub use snapshot::{
    DomainSnapshot, GitInfo, NetworkInfo, ProjectSnapshot, ResourceInfo, RestartPolicy,
    ServiceSnapshot, SnapshotServiceType, VolumeMount, VolumeType, WorkloadDescriptor, WorkloadId,
    WorkloadSnapshot, WorkloadStatus, WorkloadType,
};

// Validation types
pub use validation::{
    ImportValidationRule, ValidationLevel, ValidationReport, ValidationResult, ValidationStatus,
};
