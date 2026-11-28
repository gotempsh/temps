// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps otel` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createAlert,
  createDashboard,
  deleteAlert,
  deleteDashboard,
  getAlert,
  getCloudBackfillStatus,
  getDashboard,
  getGenaiTrace,
  getHealth,
  getPipelineStats,
  getQuota,
  hasTraces,
  listAlerts,
  listDashboards,
  listInsights,
  observabilityFullEvent,
  observabilityListEvents,
  previewAlert,
  queryGenaiTraces,
  queryLogs,
  updateAlert,
  updateDashboard,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerOtelEndpoints(program: Command): void {
  const group = topGroup(program, 'otel', "otel")

  endpoint(group, listAlerts, {
    name: "alerts list",
    description: "List alert rules for a project (newest first, paginated)",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      page: { kind: 'int', help: "Page number (default: 1)" },
      page_size: { kind: 'int', help: "Page size (default: 20, max: 100)" },
    },
  })

  endpoint(group, createAlert, {
    name: "alerts create",
    description: "Create a new alert rule for a project",
    body: {
      aggregation: { kind: 'string!', help: "One of 'avg|sum|min|max|count|rate|p50|p90|p95|p99'" },
      detection_config: { kind: 'json!', help: "The detector: a discriminated union keyed by 'kind'" },
      dynamic_alerts: { kind: 'bool', help: "When true (and 'group_by' is set) fire one independent alarm per breaching series" },
      enabled: 'bool!',
      for_duration_secs: 'int!',
      group_by: { kind: 'strings', help: "Label keys to break the metric down by, e.g. '[\"endpoint\",\"region\"]'" },
      grouped_notification_threshold: { kind: 'int', help: "When more than this many series transition to firing in the same tick, only the first gets the expensive..." },
      label_filters: { kind: 'json', help: "AND-combined label equality filters: '[[\"key\",\"value\"],…]'" },
      max_series: { kind: 'int', help: "Cardinality cap for dynamic alerting: at most this many series (top by '|value|')" },
      metric_name: { kind: 'string', help: "The OTel metric to watch" },
      name: 'string!',
      project_id: 'int!',
      severity: { kind: 'string!', help: "One of 'info|warning|critical'" },
      source: { kind: 'json', help: "What the rule watches: the metric above (the default), or the number of log lines matching a saved search..." },
      window_secs: 'int!',
    },
  })

  endpoint(group, previewAlert, {
    name: "alerts preview",
    description: "Backtest an anomaly detector over a time range without saving a rule",
    body: {
      aggregation: { kind: 'string!', help: "One of 'avg|sum|min|max|count|rate|p50|p90|p95|p99'" },
      detection_config: { kind: 'json!', help: "The detector to backtest. 'static' and 'anomaly' are supported — the kinds the evaluator actually runs" },
      end_time: { kind: 'string', help: "RFC 3339; defaults to now" },
      metric_name: 'string!',
      project_id: 'int!',
      start_time: { kind: 'string', help: "RFC 3339; defaults to 7 days before 'end_time'" },
      window_secs: 'int!',
    },
  })

  endpoint(group, deleteAlert, {
    name: "alerts delete",
    description: "Delete an alert rule",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the delete)" },
    },
    confirm: "This will delete an alert rule.",
  })

  endpoint(group, getAlert, {
    name: "alerts get",
    description: "Fetch a single alert rule by id",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the lookup)" },
    },
  })

  endpoint(group, updateAlert, {
    name: "alerts update",
    description: "Update an alert rule's fields",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the update)" },
    },
    body: {
      aggregation: 'string',
      detection_config: { kind: 'json', help: "The typed detector definition stored (as jsonb) in 'metric_alert_rules.detection_config'" },
      dynamic_alerts: { kind: 'bool', help: "Toggles per-series (\"dynamic\") alerting (absent = leave unchanged)" },
      enabled: 'bool',
      for_duration_secs: 'int',
      group_by: { kind: 'strings', help: "Replaces the group_by keys wholesale when present (absent = leave unchanged)" },
      grouped_notification_threshold: { kind: 'int', help: "Updates the notification-grouping threshold (absent = leave unchanged)" },
      label_filters: { kind: 'json', help: "Replaces the label filters wholesale when present (absent = leave unchanged)" },
      max_series: { kind: 'int', help: "Updates the dynamic-alerting cardinality cap (absent = leave unchanged)" },
      metric_name: 'string',
      name: 'string',
      severity: 'string',
      source: { kind: 'json', help: "What series a rule is evaluated on (ADR 108 D1)" },
      window_secs: 'int',
    },
  })

  endpoint(group, getCloudBackfillStatus, {
    name: "get-cloud-backfill-status",
    description: "Status of the most recent Temps Cloud telemetry backfill for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, listDashboards, {
    name: "dashboards list",
    description: "List dashboards for a project (newest first, paginated)",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      page: { kind: 'int', help: "Page number (default: 1)" },
      page_size: { kind: 'int', help: "Page size (default: 20, max: 100)" },
    },
  })

  endpoint(group, createDashboard, {
    name: "dashboards create",
    description: "Create a new dashboard for a project",
    body: {
      layout: { kind: 'json!', help: "The typed layout persisted (as JSONB) in 'metric_dashboards.layout'" },
      name: 'string!',
      project_id: 'int!',
    },
  })

  endpoint(group, deleteDashboard, {
    name: "dashboards delete",
    description: "Delete a dashboard",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the delete)" },
    },
    confirm: "This will delete a dashboard.",
  })

  endpoint(group, getDashboard, {
    name: "dashboards get",
    description: "Fetch a single dashboard by id",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the lookup)" },
    },
  })

  endpoint(group, updateDashboard, {
    name: "dashboards update",
    description: "Update a dashboard's name and/or layout",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Owning project ID (scopes the update)" },
    },
    body: {
      layout: { kind: 'json', help: "The typed layout persisted (as JSONB) in 'metric_dashboards.layout'" },
      name: 'string',
    },
  })

  endpoint(group, queryGenaiTraces, {
    name: "genai query-traces",
    description: "Query GenAI trace summaries — traces containing spans with 'gen_ai.*' attributes",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      service_name: { kind: 'string', help: "Filter by service name" },
      gen_ai_system: { kind: 'string', help: "Filter by AI system (openai, anthropic, etc.)" },
      gen_ai_model: { kind: 'string', help: "Filter by model (gpt-6.2-sol, claude-sonnet-5-5, etc.)" },
      start_time: { kind: 'string', help: "Start time (RFC 3339)" },
      end_time: { kind: 'string', help: "End time (RFC 3339)" },
      limit: { kind: 'int', help: "Max traces to return (default: 50, max: 100)" },
      offset: { kind: 'int', help: "Offset for pagination" },
    },
  })

  endpoint(group, getGenaiTrace, {
    name: "genai get-trace",
    description: "Get GenAI span details for a specific trace",
    path: { project_id: 'project', trace_id: 'string' },
    query: {
      start_time: { kind: 'string', help: "Window start (RFC 3339); pair with end_time, max 31 days" },
      end_time: { kind: 'string', help: "Window end (RFC 3339); pair with start_time, max 31 days" },
    },
  })

  endpoint(group, hasTraces, {
    name: "has-traces",
    description: "Whether a project has ever received at least one trace span",
    path: { project_id: 'project' },
  })

  endpoint(group, getHealth, {
    name: "get-health",
    description: "Get health summaries for a project",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Filter by environment ID" },
    },
  })

  endpoint(group, listInsights, {
    name: "insights list",
    description: "List anomaly insights for a project",
    path: { project_id: 'project' },
    query: {
      status: { kind: 'string', help: "Filter by status (active, resolved)" },
      limit: { kind: 'int', help: "Max insights to return (default: 20, max: 100)" },
      offset: { kind: 'int', help: "Offset for pagination" },
    },
  })

  endpoint(group, queryLogs, {
    name: "logs query",
    description: "Query log records with optional filters",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      severity: { kind: 'string', help: "Filter by severity (TRACE, DEBUG, INFO, WARN, ERROR, FATAL)" },
      service_name: { kind: 'string', help: "Filter by service name" },
      search: { kind: 'string', help: "Full-text search in log body (ILIKE)" },
      attribute_key: { kind: 'string', help: "Exact attribute key; pair with attribute_value" },
      attribute_value: { kind: 'string', help: "Exact string or numeric text, preserving int64 precision" },
      attribute_type: { kind: 'string', help: "Optional native declared type; older records without type metadata do not match", values: ["string","boolean","integer","double","array","object","bytes"] },
      attribute_op: { kind: 'string', help: "Default eq; numeric types also support lt/lte/gt/gte", values: ["eq","ne","lt","lte","gt","gte"] },
      trace_id: { kind: 'string', help: "Filter by correlated trace ID" },
      workload_kind: { kind: 'string', help: "Only what the platform recorded about one workload of the project (sandbox, deployment, ...); pair with workload_id" },
      workload_id: { kind: 'int', help: "The workload's numeric id; pair with workload_kind" },
      start_time: { kind: 'string', help: "Start time (RFC 3339)" },
      end_time: { kind: 'string', help: "End time (RFC 3339)" },
      limit: { kind: 'int', help: "Max logs to return (default: 100, max: 1000)" },
      offset: { kind: 'int', help: "Offset for pagination" },
    },
  })

  endpoint(group, getPipelineStats, {
    name: "get-pipeline-stats",
    description: "Get OTel pipeline statistics (admin/system view)",
  })

  endpoint(group, getQuota, {
    name: "get-quota",
    description: "Get storage quota for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, observabilityListEvents, {
    name: "observe observability-list-events",
    description: "List a merged page of observability events for a project",
    path: { project_id: 'project' },
    query: {
      kinds: { kind: 'string', help: "Comma-separated kinds: 'request,span,error,revenue'" },
      from: { kind: 'string', help: "Inclusive lower bound on event timestamp (ISO 8601, 'Z' suffix)" },
      to: { kind: 'string', help: "Inclusive upper bound on event timestamp" },
      deployment_id: 'int',
      environment_id: 'int',
      search: { kind: 'string', help: "Free-text substring matched against per-kind summary fields (request path / error class / revenue event_type)" },
      limit: { kind: 'int', help: "Page size (default 50, max 200)" },
      hide_bots: { kind: 'bool', help: "When 'true', exclude bot/crawler request rows" },
    },
  })

  endpoint(group, observabilityFullEvent, {
    name: "observe observability-full-event",
    description: "Fetch the un-truncated form of one event by '(kind, id)'",
    path: { project_id: 'project', kind: 'string', event_id: 'string' },
    query: {
      ts: { kind: 'string', help: "The row's event timestamp as returned by the list endpoint" },
    },
  })
}
