// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress (ADR-055 §11): endpoints, hostname claims and access policies.
//!
//! - [`endpoint`]: the model. An endpoint belongs to a product object, not
//!   to a workload revision, so blue/green is one write to its target.
//! - [`host`]: hostname syntax, normalisation and the platform's reserved
//!   hosts (ADR-051).
//! - [`admission`]: what an endpoint may be, checked before it is stored.
//! - [`compiled`]: claims and endpoints compiled into a route table that
//!   answers `(host, path) → endpoint` and evaluates access policies with
//!   no locks, no I/O and no allocation. It is built on the control plane
//!   and swapped in behind the proxy's `ArcSwap`; every decision is made
//!   from the snapshot alone, because workers have no database.

pub mod admission;
pub mod compiled;
pub mod endpoint;
pub mod host;

pub use admission::{admit, AdmissionError};
pub use compiled::{
    Challenge, ClaimConflict, CompiledEndpoint, CompiledRoutes, Decision, DenyReason,
    RequestContext,
};
pub use endpoint::{
    AccessPolicy, Endpoint, EndpointProtocol, EndpointTarget, HostClaim, HostClaimKind, IpRule,
    ProductKind, ProductRef, RateLimit,
};
pub use host::{normalize_host, HostError, ReservedHosts};
