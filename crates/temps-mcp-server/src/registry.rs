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

/// Collect the MCP tools that match the requested groups.
///
/// `requested_groups` is the parsed `?groups=` value (empty = all groups).
/// `write_enabled` gates write tools in each group.
pub fn collect_tools(requested_groups: &[String], write_enabled: bool) -> Vec<McpTool> {
    let all = requested_groups.is_empty();
    let wants = |key: &str| all || requested_groups.iter().any(|g| g == key);

    let mut tools = Vec::new();

    if wants("deployments") {
        tools.extend(crate::tools::deployments::tools(write_enabled));
    }
    if wants("infrastructure") {
        // TODO(mcp): implement infrastructure tools (services, containers, load-balancer, scans)
    }
    if wants("networking") {
        // TODO(mcp): implement networking tools (domains, custom-domains, dns-providers, ip-access)
    }
    if wants("data") {
        // TODO(mcp): implement data tools (backups, dsn)
    }
    if wants("observability") {
        // TODO(mcp): implement observability tools (monitors, incidents, errors, proxy-logs, funnels, analytics)
    }
    if wants("notifications") {
        // TODO(mcp): implement notifications tools (notifications, notification-prefs, webhooks, email-domains, email-providers)
    }
    if wants("platform") {
        tools.extend(crate::tools::platform::tools());
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_groups_constant_has_seven_entries() {
        assert_eq!(ALL_GROUP_INFOS.len(), 7);
        let keys: Vec<&str> = ALL_GROUP_INFOS.iter().map(|g| g.key).collect();
        assert!(keys.contains(&"deployments"));
        assert!(keys.contains(&"platform"));
    }

    #[test]
    fn empty_requested_groups_returns_all() {
        let tools = collect_tools(&[], false);
        // At minimum platform + deployments read tools are present.
        assert!(!tools.is_empty());
    }

    #[test]
    fn single_group_filter() {
        let platform_only = collect_tools(&["platform".to_string()], false);
        let names: Vec<&str> = platform_only.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"list_projects"));
        assert!(!names.contains(&"list_deployments"));
    }

    #[test]
    fn write_mode_adds_write_tools() {
        let with_write = collect_tools(&["deployments".to_string()], true);
        let without_write = collect_tools(&["deployments".to_string()], false);
        assert!(with_write.len() > without_write.len());
        let names_write: Vec<&str> = with_write.iter().map(|t| t.name.as_str()).collect();
        assert!(names_write.contains(&"trigger_deployment"));
    }
}
