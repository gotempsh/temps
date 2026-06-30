//! `temps-ai-api-tools` — Phase 1 CORE of ADR-024.
//!
//! Provides [`InternalApiCaller`]: a substrate-agnostic component that turns the
//! read-only REST surface (derived from a `utoipa::openapi::OpenApi` document) into a
//! searchable, callable index.  Each call is executed by replaying a synthetic GET
//! through the real Axum [`axum::Router`] with the caller's [`AuthContext`] injected
//! into request extensions — so the router's `permission_guard!`, project scoping,
//! and DTOs remain the single security enforcement point.
//!
//! ## Design
//!
//! 1. [`ReadOnlyApiIndex`] filters the OpenAPI document down to GET-only operations,
//!    builds an in-memory index, and supports keyword search.
//! 2. [`build_request_parts`] performs pure param routing/validation: path-template
//!    substitution, query-string construction, enum membership, project-id scoping,
//!    and limit clamping.
//! 3. [`InternalApiCaller`] wires the index and a cloneable [`axum::Router`] together
//!    and executes calls via `tower::ServiceExt::oneshot`.
//! 4. [`ApiToolsHandle`] is a shared, lazily-set holder for the constructed caller;
//!    registered as a service at plugin init and populated after the router is built
//!    in `console.rs`.
//!
//! See ADR-024 for the full design rationale and phased plan.

mod caller;
mod cli;
mod error;
mod handle;
mod index;
mod integration_tests;

pub use caller::{ApiCallScope, ApiToolResponse, BuiltRequest, InternalApiCaller};
pub use error::ApiToolError;
pub use handle::ApiToolsHandle;
pub use index::{
    ApiOperation, OperationSchema, OperationSummary, ParamLocation, ParamSpec, ReadOnlyApiIndex,
};

/// Build the request path+query string from a flat params object.
///
/// This is a pure function (no I/O) re-exported at the crate root so tests
/// and adapters can call it without constructing a full [`InternalApiCaller`].
pub use caller::build_request_parts;
