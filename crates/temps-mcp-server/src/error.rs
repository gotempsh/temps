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
    #[error("Virtual CLI is not initialised")]
    CliUnavailable,
    #[error("Virtual CLI request failed: {}", temps_ai_api_tools::redaction::redact_text(&source.to_string()))]
    Cli {
        #[from]
        source: temps_ai_api_tools::ApiToolError,
    },
    #[error("Proposal belongs to a different API key or user")]
    ProposalOwnerMismatch,

    /// Feature flag is off — the operator has not enabled MCP in Settings.
    #[error("MCP server is disabled; enable it in Settings → Platform → MCP Server")]
    FeatureDisabled,

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

    /// The `jsonrpc` field in the request was not the required value `"2.0"`.
    #[error("Invalid JSON-RPC version '{received}'; this server requires jsonrpc == \"2.0\"")]
    InvalidJsonRpcVersion { received: String },

    /// The tool name supplied in tools/call does not exist.
    #[error("Unknown tool '{name}'; use temps with command 'temps --help'")]
    UnknownTool { name: String },

    /// A required argument was absent from the tool arguments object.
    #[error("Missing required argument '{arg}' for tool '{tool}'")]
    MissingArgument { arg: String, tool: String },

    /// An argument was present but had an invalid value.
    #[error("Invalid argument '{arg}': {reason}")]
    InvalidArgument { arg: String, reason: String },

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
            Self::CliUnavailable => crate::protocol::INTERNAL_ERROR,
            Self::Cli {
                source: temps_ai_api_tools::ApiToolError::Upstream { status: 403, .. },
            } => -32006,
            Self::Cli {
                source:
                    temps_ai_api_tools::ApiToolError::Upstream {
                        status: 500..=599, ..
                    }
                    | temps_ai_api_tools::ApiToolError::RouterError { .. }
                    | temps_ai_api_tools::ApiToolError::BodyReadError { .. },
            } => crate::protocol::INTERNAL_ERROR,
            Self::Cli { .. } => crate::protocol::INVALID_PARAMS,
            Self::ProposalOwnerMismatch => -32005,
            Self::FeatureDisabled => -32001,
            Self::ProposalNotFound | Self::ProposalExpired => -32003,
            Self::WriteNotEnabled => -32004,
            Self::InvalidJsonRpcVersion { .. } => crate::protocol::INVALID_REQUEST,
            Self::UnknownTool { .. } => crate::protocol::METHOD_NOT_FOUND,
            Self::MissingArgument { .. } | Self::InvalidArgument { .. } => {
                crate::protocol::INVALID_PARAMS
            }
            Self::Config(_) | Self::Serialization(_) => crate::protocol::INTERNAL_ERROR,
        }
    }
}

impl From<McpError> for Problem {
    fn from(error: McpError) -> Self {
        match error {
            McpError::CliUnavailable => problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("CLI Unavailable")
                .with_detail(error.to_string()),
            McpError::Cli { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("CLI Request Failed")
                .with_detail(error.to_string()),
            McpError::ProposalOwnerMismatch => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Proposal Owner Mismatch")
                .with_detail(error.to_string()),
            McpError::FeatureDisabled => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("MCP Server Disabled")
                .with_detail(error.to_string()),

            McpError::ProposalNotFound | McpError::ProposalExpired => {
                problemdetails::new(StatusCode::GONE)
                    .with_title("Proposal Unavailable")
                    .with_detail(error.to_string())
            }

            McpError::WriteNotEnabled => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Write Mode Disabled")
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

            McpError::Config(_) | McpError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_ai_api_tools::ApiToolError;

    #[test]
    fn cli_failures_keep_server_permission_and_argument_classes_distinct() {
        for (status, expected) in [
            (400, -32602),
            (403, -32006),
            (422, -32602),
            (500, -32603),
            (503, -32603),
        ] {
            let error = McpError::from(ApiToolError::Upstream {
                status,
                detail: "fixture failure".into(),
                operation_id: "get_projects".into(),
            });
            assert_eq!(error.rpc_code(), expected, "HTTP {status}");
        }
        for source in [
            ApiToolError::RouterError {
                operation_id: "get_projects".into(),
                reason: "unavailable".into(),
            },
            ApiToolError::BodyReadError {
                operation_id: "get_projects".into(),
                reason: "incomplete body".into(),
            },
        ] {
            assert_eq!(
                McpError::from(source).rpc_code(),
                crate::protocol::INTERNAL_ERROR
            );
        }
        assert_eq!(
            McpError::from(ApiToolError::InvalidCommand {
                detail: "use --help".into()
            })
            .rpc_code(),
            crate::protocol::INVALID_PARAMS
        );
    }
}
