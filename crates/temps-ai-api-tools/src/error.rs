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

    /// Several required parameters were absent at once.
    ///
    /// Reported together rather than one-at-a-time on purpose: the model gets a
    /// single round to fix the whole call. Naming only the first missing field
    /// costs one model round (and one wasted proposal) per field, so an
    /// operation with eight required fields — `create_alert` is one — can
    /// exhaust the tool loop's round cap before it ever manages a valid call.
    #[error(
        "Missing required parameters for operation '{operation_id}': {names}. \
         Provide ALL of them in the flat parameters object."
    )]
    MissingParams {
        /// Comma-separated names of every missing required parameter.
        names: String,
        /// The operation that required them.
        operation_id: String,
    },

    /// One or more supplied parameters are not part of the operation.
    ///
    /// This has to be an error, not a shrug. An unrecognised parameter used to
    /// be dropped on the floor, which quietly turned a *filtered* query into an
    /// unfiltered one: `query_metrics --metric http.server.duration` (the flag
    /// is `--metric_name`) returned a 200 containing the average across every
    /// metric in the project, a plausible-looking number that answers a
    /// different question. Nothing downstream can detect that, so the model
    /// reasons confidently on nonsense — the worst possible failure mode for a
    /// tool whose output becomes a threshold someone gets paged by.
    #[error(
        "Unknown parameter(s) for operation '{operation_id}': {unknown}. Valid parameters: \
         {valid}. Nothing was queried — re-run with the correct name."
    )]
    UnknownParams {
        /// The offending names, with a suggestion where one is obvious.
        unknown: String,
        /// Every parameter the operation does accept.
        valid: String,
        /// The operation being called.
        operation_id: String,
    },

    /// An object-valued body parameter does not match its schema's shape.
    ///
    /// Caught during validation rather than at execution, which matters most on
    /// the propose-then-confirm write path: without this check a structurally
    /// impossible call is staged, a human approves it, and only then does the
    /// API reject it — the one person who could not have known better pays for
    /// the mistake. The message carries the accepted shape because the model
    /// does not reliably consult `--help` first.
    #[error(
        "Parameter '{name}' has the wrong shape for operation '{operation_id}': {problem}. \
         Expected {expected}."
    )]
    BadObjectShape {
        /// Name of the offending parameter.
        name: String,
        /// What is wrong with the supplied value.
        problem: String,
        /// Human-readable description of the accepted JSON.
        expected: String,
        /// The operation being called.
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
