//! Error types for `temps-ai-api-tools`.
//!
//! Every variant carries sufficient context (names, IDs, allowed values) so the
//! calling LLM agent can self-correct in a single round-trip.

use thiserror::Error;

/// All errors that can arise when searching, describing, or calling an API tool.
#[derive(Error, Debug, Clone)]
pub enum ApiToolError {
    /// A required parameter was absent from the supplied `params` object.
    #[error(
        "Required parameter '{name}' is missing for operation '{operation_id}'. \
         Provide it in the flat parameters object."
    )]
    MissingParam {
        /// Name of the missing parameter.
        name: String,
        /// The operation that required it.
        operation_id: String,
    },

    /// A parameter value is not one of the declared enum members.
    #[error(
        "Parameter '{name}' has value '{value}' which is not one of the allowed values: \
         [{allowed}] for operation '{operation_id}'."
    )]
    BadEnum {
        /// Name of the offending parameter.
        name: String,
        /// The value that was supplied.
        value: String,
        /// Comma-separated list of allowed values.
        allowed: String,
        /// The operation being called.
        operation_id: String,
    },

    /// The `project_id` value supplied (or inferred) is not in the caller's
    /// accessible project list.  This is a scoping error, not an authz error — the
    /// real authz is enforced by the router, but we prevent trivially wrong calls.
    #[error(
        "project_id {project_id} is not in the caller's accessible project list \
         [{allowed}] for operation '{operation_id}'. \
         Use one of the listed project IDs."
    )]
    ProjectNotAllowed {
        /// The project_id value that was attempted.
        project_id: i32,
        /// Comma-separated list of accessible project IDs.
        allowed: String,
        /// The operation being called.
        operation_id: String,
    },

    /// The requested `operation_id` does not exist in the read-only index.
    #[error(
        "Operation '{operation_id}' was not found in the read-only API index. \
         Use search_api to discover available operations."
    )]
    UnknownOperation {
        /// The operation_id that was looked up.
        operation_id: String,
    },

    /// The operation exists in the OpenAPI document but is not a GET (not read-only).
    /// This should not normally occur because the index only contains GETs, but is
    /// included for safety.
    #[error(
        "Operation '{operation_id}' is not a read-only GET operation and cannot be \
         called through this interface."
    )]
    NotReadOnly {
        /// The operation that was attempted.
        operation_id: String,
    },

    /// The upstream router returned a non-success HTTP status.
    #[error("Upstream call to operation '{operation_id}' returned HTTP {status}: {detail}")]
    Upstream {
        /// The HTTP status code returned.
        status: u16,
        /// Human-readable detail from the response body (possibly truncated).
        detail: String,
        /// The operation that was called.
        operation_id: String,
    },

    /// The router itself produced an error (e.g. service unavailable during test).
    #[error("Router error calling operation '{operation_id}': {reason}")]
    RouterError {
        /// The operation that was being called.
        operation_id: String,
        /// Description of the router error.
        reason: String,
    },

    /// The response body exceeded the configured size cap and could not be
    /// collected even with truncation (i.e., the cap itself is unreachable).
    #[error("Failed to read response body for operation '{operation_id}': {reason}")]
    BodyReadError {
        /// The operation that was called.
        operation_id: String,
        /// What went wrong reading the body.
        reason: String,
    },
}
