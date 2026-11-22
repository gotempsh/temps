// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::protocol::{McpTool, ToolGroupInfo};

/// Static metadata for all 7 tool groups (mirrors groups.ts in the CLI).
pub const ALL_GROUP_INFOS: &[ToolGroupInfo] = &[
    ToolGroupInfo {
        key: "deployments",
        label: "Deployments & Projects",
    },
    ToolGroupInfo {
        key: "infrastructure",
        label: "Infrastructure",
    },
    ToolGroupInfo {
        key: "networking",
        label: "Networking & Domains",
    },
    ToolGroupInfo {
        key: "data",
        label: "Databases & Backups",
    },
    ToolGroupInfo {
        key: "observability",
        label: "Observability",
    },
    ToolGroupInfo {
        key: "notifications",
        label: "Notifications",
    },
    ToolGroupInfo {
        key: "platform",
        label: "Platform & Access",
    },
];

/// The virtual CLI is independent of legacy `?groups=` filters. Keep accepting
/// them so existing connection URLs can migrate without reconfiguration.
pub fn collect_tools(_requested_groups: &[String], write_enabled: bool) -> Vec<McpTool> {
    crate::virtual_cli::tools(write_enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_virtual_cli_tools_are_listed_regardless_of_legacy_filter() {
        for groups in [
            vec![],
            vec!["platform".into()],
            vec!["networking".into()],
            vec!["unknown".into()],
        ] {
            let read = collect_tools(&groups, false);
            assert_eq!(
                read.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                ["temps"]
            );
            let write = collect_tools(&groups, true);
            assert_eq!(
                write.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                ["temps", "temps_write", "confirm_action"]
            );
        }
    }
    #[test]
    fn migrated_operations_use_shared_chat_allowlists() {
        let reads = temps_ai_api_tools::allowlists::ai_read_allowlist();
        for operation in ["get_projects", "get_project", "get_project_deployments"] {
            assert!(
                reads.iter().any(|id| id == operation),
                "missing {operation}"
            );
        }
        let writes = temps_ai_api_tools::allowlists::ai_write_allowlist();
        assert!(writes.iter().any(|id| id == "trigger_project_pipeline"));
        // Edge-rule match values can contain secrets. Removal must not silently
        // widen the shared policy merely to retain the legacy tool surface.
        for operation in ["list_edge_rules", "get_edge_rule"] {
            assert!(!reads.iter().any(|id| id == operation));
        }
    }
}
