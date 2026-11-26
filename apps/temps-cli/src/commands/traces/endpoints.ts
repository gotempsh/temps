// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps traces` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getCrossProjectTraceSiblings,
  getUnifiedTrace,
  queryGlobalTraceSummaries,
  queryGlobalTraces,
  queryTraceSummaries,
  queryTraces,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerTracesEndpoints(program: Command): void {
  const group = topGroup(program, 'traces', "traces")

  endpoint(group, queryGlobalTraces, {
    name: "query-global",
    description: "Query global traces",
    query: {
      project_id: 'int',
      trace_id: 'string',
      service_name: 'string',
      status: 'string',
      min_duration_ms: 'number',
      start_time: 'string',
      end_time: 'string',
      environment_id: 'int',
      deployment_id: 'int',
      attributes: { kind: 'string', help: "Filter by span attributes as comma-separated key=value pairs. e.g...." },
      name_pattern: { kind: 'string', help: "Filter by span name pattern (ILIKE)" },
      sort_by: { kind: 'string', help: "Sort field for the trace-summaries list: \"start_time\" (default) or \"duration\"" },
      sort_order: { kind: 'string', help: "Sort direction: \"asc\" or \"desc\" (default)" },
      limit: 'int',
      offset: 'int',
    },
  })

  endpoint(group, queryGlobalTraceSummaries, {
    name: "query-global-summaries",
    description: "Query global trace summaries",
    query: {
      project_id: 'int',
      trace_id: 'string',
      service_name: 'string',
      status: 'string',
      min_duration_ms: 'number',
      start_time: 'string',
      end_time: 'string',
      environment_id: 'int',
      deployment_id: 'int',
      attributes: { kind: 'string', help: "Filter by span attributes as comma-separated key=value pairs. e.g...." },
      name_pattern: { kind: 'string', help: "Filter by span name pattern (ILIKE)" },
      sort_by: { kind: 'string', help: "Sort field for the trace-summaries list: \"start_time\" (default) or \"duration\"" },
      sort_order: { kind: 'string', help: "Sort direction: \"asc\" or \"desc\" (default)" },
      limit: 'int',
      offset: 'int',
    },
  })

  endpoint(group, getUnifiedTrace, {
    name: "get-unified",
    description: "Assemble a unified cross-project span waterfall (Phase 2)",
    path: { trace_id: 'string' },
    query: {
      start_time: { kind: 'string', help: "Window start (RFC 3339); pair with end_time, max 31 days" },
      end_time: { kind: 'string', help: "Window end (RFC 3339); pair with start_time, max 31 days" },
    },
  })

  endpoint(group, queryTraceSummaries, {
    name: "query-summaries",
    description: "Query trace summaries — one row per trace with span count, error count, root span info, and proper..",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      trace_id: { kind: 'string', help: "Filter by trace ID" },
      service_name: { kind: 'string', help: "Filter by service name" },
      status: { kind: 'string', help: "Filter by status (OK, ERROR)" },
      min_duration_ms: { kind: 'number', help: "Minimum trace duration in ms" },
      start_time: { kind: 'string', help: "Start time (RFC 3339)" },
      end_time: { kind: 'string', help: "End time (RFC 3339)" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      deployment_id: { kind: 'int', help: "Filter by deployment ID" },
      attributes: { kind: 'string', help: "Filter by span attributes as comma-separated key=value pairs, e.g...." },
      name_pattern: { kind: 'string', help: "Filter by span name pattern (ILIKE)" },
      sort_by: { kind: 'string', help: "Sort field: 'start_time' (default) or 'duration'" },
      sort_order: { kind: 'string', help: "Sort direction: 'asc' or 'desc' (default)" },
      include_total: { kind: 'bool', help: "Compute the 'total' count (default: true)" },
      limit: { kind: 'int', help: "Max traces to return (default: 50, max: 100)" },
      offset: { kind: 'int', help: "Offset for pagination" },
    },
  })

  endpoint(group, queryTraces, {
    name: "query",
    description: "Query trace spans with optional filters",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      trace_id: { kind: 'string', help: "Filter by trace ID" },
      service_name: { kind: 'string', help: "Filter by service name" },
      status: { kind: 'string', help: "Filter by status (OK, ERROR, UNSET)" },
      min_duration_ms: { kind: 'number', help: "Minimum span duration in ms" },
      start_time: { kind: 'string', help: "Start time (RFC 3339)" },
      end_time: { kind: 'string', help: "End time (RFC 3339)" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      deployment_id: { kind: 'int', help: "Filter by deployment ID" },
      attributes: { kind: 'string', help: "Filter by span attributes as comma-separated key=value pairs, e.g...." },
      name_pattern: { kind: 'string', help: "Filter by span name pattern (ILIKE)" },
      limit: { kind: 'int', help: "Max spans to return (default: 100, max: 1000)" },
      offset: { kind: 'int', help: "Offset for pagination" },
    },
  })

  endpoint(group, getCrossProjectTraceSiblings, {
    name: "get-cross-siblings",
    description: "Discover sibling projects that share the same 'trace_id' (Phase 1 banner)",
    path: { trace_id: 'string' },
    query: {
      exclude_project_id: { kind: 'int', help: "Project ID to exclude (the caller's own project) so the UI does not render a self-link" },
    },
  })
}
