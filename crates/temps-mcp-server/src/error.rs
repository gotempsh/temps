// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

/// All errors the MCP server can produce, covering both protocol-level and
/// domain-level failures.  Every variant carries enough context for the caller
/// to produce a meaningful JSON-RPC error or HTTP Problem Details response.
#[derive(Debug, Error)]
pub enum McpError {
    /// Feature flag is off — the operator has not enabled MCP in Settings.
    #[error("MCP server is disabled; enable it in Settings → Platform → MCP Server")]
    FeatureDisabled,

    /// The requested project does not exist on this instance.
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    /// The requested deployment does not exist in the given project.
    #[error("Deployment not found in project {project_id}")]
    DeploymentNotFound { project_id: i32 },

    /// The proposal token was not found or was already consumed.
    #[error("Proposal token not found or already used")]
    ProposalNotFound,

    /// The proposal token was found but the 5-minute window has passed.
    #[error("Proposal token expired; create a new proposal to retry")]
    ProposalExpired,

    /// The caller attempted a write tool but write mode is disabled.
    #[error(
        "Write operations require write=1 in the MCP URL; re-run the wizard with write mode enabled"
    )]
    WriteNotEnabled,

    /// The caller holds a token but lacks a required permission for the
    /// operation.  Distinct from `WriteNotEnabled` (which is an opt-in
    /// gate), this indicates the token's role does not carry the capability.
    #[error("This operation requires the {permission} permission")]
    InsufficientPermission { permission: String },

    /// The caller's token is valid but they are not allowed to access the
    /// given project (per the registered `ProjectAccessChecker`).
    #[error("Access to project {project_id} is denied for this token")]
    ProjectAccessDenied { project_id: i32 },

    /// The `jsonrpc` field in the request was not the required value `"2.0"`.
    #[error("Invalid JSON-RPC version '{received}'; this server requires jsonrpc == \"2.0\"")]
    InvalidJsonRpcVersion { received: String },

    /// The tool name supplied in tools/call does not exist.
    #[error("Unknown tool '{name}'")]
    UnknownTool { name: String },

    /// A required argument was absent from the tool arguments object.
    #[error("Missing required argument '{arg}' for tool '{tool}'")]
    MissingArgument { arg: String, tool: String },

    /// An argument was present but had an invalid value.
    #[error("Invalid argument '{arg}': {reason}")]
    InvalidArgument { arg: String, reason: String },

    /// The upstream ProjectService returned an error.
    #[error("Project service error: {0}")]
    ProjectService(String),

    /// The upstream DeploymentService returned an error.
    #[error("Deployment service error: {0}")]
    DeploymentService(String),

    /// The ConfigService failed to load settings.
    #[error("Config error: {0}")]
    Config(String),

    /// JSON serialization failed unexpectedly.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl McpError {
    /// MCP JSON-RPC error code appropriate for this variant.
    pub fn rpc_code(&self) -> i32 {
        match self {
            Self::FeatureDisabled => -32001,
            Self::ProjectNotFound { .. } | Self::DeploymentNotFound { .. } => -32002,
            Self::ProposalNotFound | Self::ProposalExpired => -32003,
            Self::WriteNotEnabled => -32004,
            Self::ProjectAccessDenied { .. } => -32005,
            Self::InsufficientPermission { .. } => -32006,
            Self::InvalidJsonRpcVersion { .. } => crate::protocol::INVALID_REQUEST,
            Self::UnknownTool { .. } => crate::protocol::METHOD_NOT_FOUND,
            Self::MissingArgument { .. } | Self::InvalidArgument { .. } => {
                crate::protocol::INVALID_PARAMS
            }
            Self::ProjectService(_)
            | Self::DeploymentService(_)
            | Self::Config(_)
            | Self::Serialization(_) => crate::protocol::INTERNAL_ERROR,
        }
    }
}

impl From<McpError> for Problem {
    fn from(error: McpError) -> Self {
        match error {
            McpError::FeatureDisabled => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("MCP Server Disabled")
                .with_detail(error.to_string()),

            McpError::ProjectNotFound { .. } | McpError::DeploymentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Resource Not Found")
                    .with_detail(error.to_string())
            }

            McpError::ProposalNotFound | McpError::ProposalExpired => {
                problemdetails::new(StatusCode::GONE)
                    .with_title("Proposal Unavailable")
                    .with_detail(error.to_string())
            }

            McpError::WriteNotEnabled => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Write Mode Disabled")
                .with_detail(error.to_string()),

            McpError::InsufficientPermission { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Insufficient Permission")
                .with_detail(error.to_string()),

            McpError::ProjectAccessDenied { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Project Access Denied")
                .with_detail(error.to_string()),

            McpError::InvalidJsonRpcVersion { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid JSON-RPC Version")
                .with_detail(error.to_string()),

            McpError::UnknownTool { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Unknown Tool")
                .with_detail(error.to_string()),

            McpError::MissingArgument { .. } | McpError::InvalidArgument { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Tool Arguments")
                    .with_detail(error.to_string())
            }

            McpError::ProjectService(_)
            | McpError::DeploymentService(_)
            | McpError::Config(_)
            | McpError::Serialization(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}
