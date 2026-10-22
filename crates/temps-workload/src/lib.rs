// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Leaf types of the workload plane (ADR-052 §1, ADR-055 §2-§6, §18).
//!
//! A *workload* is anything Temps runs on a user's behalf. This crate holds
//! what every part of the plane agrees on and nothing that does I/O:
//!
//! - [`WorkloadRef`]: the identity rules, names and labels attach to;
//! - [`WorkloadSpec`]: the desired state a product submits;
//! - [`SecurityProfile`] and [`Resources`]: the admission inputs;
//! - [`Condition`] and [`WorkloadEvent`]: how the plane explains state;
//! - [`RuntimeDriver`]: the one trait a runtime implements.
//!
//! It depends on nothing in Temps, so drivers, concern crates
//! (`temps-workload-env`, `-netpol`, `-ingress`, `-volumes`) and products
//! can all use it without depending on each other.

pub mod condition;
pub mod driver;
pub mod labels;
pub mod reference;
pub mod resources;
pub mod spec;

pub use condition::{
    Condition, ConditionKind, ConditionStatus, WorkloadEvent, WorkloadPhase, WorkloadStatus,
};
pub use driver::{
    ArtifactKinds, AttachmentKind, Attachments, DriverCapabilities, DriverError, ExecSpec,
    ExecStream, InstanceHandle, InstanceLimits, InstanceSpec, InstanceState, InstanceStats,
    InstanceStatus, LimitSupport, LogLine, LogQuery, LogRotation, LogStream, NetworkAttachment,
    PortBinding, PreparedArtifact, RuntimeDriver, SecretFile, SnapshotRef, VolumeKinds,
};
pub use reference::{InvalidWorkloadRef, UnknownWorkloadKind, WorkloadKind, WorkloadRef};
pub use resources::{ResourceAmounts, Resources};
pub use spec::{
    AppProtocol, Artifact, Delivery, EgressProfile, EnvSpec, Isolation, Lifetime, NetworkSpec,
    PlacementSpec, PortProtocol, PortSpec, Probe, ProbeKind, Probes, Process, RestartPolicy,
    SecretRef, SecurityProfile, VolumeMount, WorkloadSpec,
};
