// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kubernetes Workload Importer
//!
//! Implements the `WorkloadImporter` trait for Kubernetes clusters, accessed
//! via a user-supplied kubeconfig (passed per-request in
//! `ImportCredentials.extra["kubeconfig"]` — never persisted).
//!
//! # What it does
//!
//! - **Discover**: lists Deployments, StatefulSets, and CronJobs across
//!   non-system namespaces.
//! - **Describe**: turns one workload into a [`temps_import_types::WorkloadSnapshot`]
//!   (image, env resolved through ConfigMaps/Secrets, ports via matching
//!   Services, volumes, resources, probes).
//! - **Project snapshot**: treats the workload's namespace as the project
//!   boundary — sibling database StatefulSets become managed-service
//!   candidates, Ingress hosts become domains, other workloads become
//!   additional deployments. It also captures a cluster-wide observation
//!   (nodes, resource requests, live usage from `metrics.k8s.io`).
//! - **Plan**: generates a reviewable migration plan **including a cost +
//!   overprovisioning analysis**: what the cluster costs today, how little of
//!   it is actually used, and what the same workloads would cost on a
//!   Hetzner server running temps.
//!
//! # Authentication
//!
//! Supported kubeconfig auth: bearer token, embedded client certificates,
//! and basic auth. `exec` plugins (EKS `aws eks get-token`, GKE auth
//! plugins) cannot run server-side — the error message tells the user to
//! mint a ServiceAccount token instead (`kubectl create token`).

pub mod client;
pub mod cluster;
pub mod cost;
pub mod error;
pub mod importer;
pub mod kubeconfig;
pub mod model;
pub mod validation;

pub use error::KubernetesImportError;
pub use importer::KubernetesImporter;
