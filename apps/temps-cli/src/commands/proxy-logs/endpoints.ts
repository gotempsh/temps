// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps proxy-logs` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getAiAgentPages,
  getAiAgentTimeline,
  getAiStatusBreakdown,
  getApiTrafficProxyLogAccess,
  getProjectsHealth,
  listKnownAiAgents,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerProxyLogsEndpoints(program: Command): void {
  const group = topGroup(program, 'proxy-logs', "proxy-logs")

  endpoint(group, getApiTrafficProxyLogAccess, {
    name: "get-api-traffic-access",
    description: "Report whether the current principal may drill into a project's proxy logs",
    path: { project_id: 'project' },
  })

  endpoint(group, listKnownAiAgents, {
    name: "list-known-ai-agents",
    description: "List every AI agent the detector knows how to classify",
  })

  endpoint(group, getAiAgentPages, {
    name: "get-ai-agent-pages",
    description: "Get the top pages accessed by a specific AI agent over a time window",
    query: {
      agent: { kind: 'string!', help: "Canonical agent name to filter by (e.g. 'ChatGPT-User', 'ClaudeBot')" },
      project_id: { kind: 'int', help: "Filter by project ID" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      start_time: { kind: 'string', help: "Start time (ISO 8601)" },
      end_time: { kind: 'string', help: "End time (ISO 8601)" },
      limit: { kind: 'int', help: "Maximum rows to return" },
    },
  })

  endpoint(group, getAiAgentTimeline, {
    name: "get-ai-agent-timeline",
    description: "Time-bucketed AI-agent request volume, split by provider or agent",
    query: {
      project_id: { kind: 'int', help: "Filter by project ID" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      start_time: { kind: 'string', help: "Start time (ISO 8601)" },
      end_time: { kind: 'string', help: "End time (ISO 8601)" },
      group_by: { kind: 'string', help: "Grouping dimension: 'provider' (default) or 'agent'" },
      bucket: { kind: 'string', help: "Bucket interval override (e.g. '1 hour', '1 day')" },
    },
  })

  endpoint(group, getAiStatusBreakdown, {
    name: "get-ai-status-breakdown",
    description: "HTTP status-class breakdown for AI-agent traffic — are bots being served (2xx) or hitting broken/blocked..",
    query: {
      project_id: { kind: 'int', help: "Filter by project ID (recommended for per-project analytics)" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      start_time: { kind: 'string', help: "Start time (ISO 8601)" },
      end_time: { kind: 'string', help: "End time (ISO 8601)" },
      limit: { kind: 'int', help: "Maximum rows to return" },
      path: { kind: 'string', help: "Optional exact path filter" },
    },
  })

  endpoint(group, getProjectsHealth, {
    name: "get-health",
    description: "Get user-traffic health summaries for multiple projects (last 1 hour)",
    query: {
      project_ids: { kind: 'string!', help: "Comma-separated list of project IDs" },
      start_time: { kind: 'string', help: "Optional start time (ISO 8601)" },
      end_time: { kind: 'string', help: "Optional end time (ISO 8601)" },
      is_bot: { kind: 'bool', help: "Filter by bot detection" },
    },
  })
}
