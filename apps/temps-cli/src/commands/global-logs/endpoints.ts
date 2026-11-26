// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps logs` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getLogContext,
  getSavedSearch,
  globalLogAggregate,
  globalLogAttributeKeys,
  globalLogCapabilities,
  globalLogFacetsAttrs,
  globalLogHistogram,
  ingestLogs,
  logIngestBudget,
  purgeProjectLogs,
  tailLogs,
  updateSavedSearch,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerGlobalLogsEndpoints(program: Command): void {
  const group = topGroup(program, 'logs', "logs")

  endpoint(group, getLogContext, {
    name: "get-context",
    description: "Get context lines surrounding a specific log line",
    query: {
      timestamp: { kind: 'string!', help: "Target line timestamp (RFC 3339)" },
      container_id: { kind: 'string!', help: "Target line container ID" },
      line_id: { kind: 'string!', help: "Target line_id, as a decimal string" },
      lines: { kind: 'int', help: "Context lines before and after (default: 25, max 50)" },
    },
  })

  endpoint(group, globalLogAggregate, {
    name: "global-aggregate",
    description: "Global log aggregate",
    query: {
      start_time: { kind: 'string!', help: "Window start (RFC 3339)" },
      end_time: { kind: 'string!', help: "Window end (RFC 3339)" },
      source: { kind: 'string', help: "Source kind (default: all)", values: ["collected","application","service"] },
      projects: { kind: 'strings', help: "Project selectors (repeatable)" },
      external_services: { kind: 'strings', help: "External service selectors (repeatable)" },
      scopes: { kind: 'strings', help: "Scope selectors (repeatable)" },
      levels: { kind: 'strings', help: "Levels (repeatable)", values: ["TRACE","DEBUG","INFO","WARN","ERROR"] },
      envs: { kind: 'strings', help: "Environments (repeatable)" },
      services: { kind: 'strings', help: "Services (repeatable)" },
      container_ids: { kind: 'strings', help: "Container ids (repeatable)" },
      node_ids: { kind: 'ints', help: "Node ids (repeatable)" },
      deploy_id: { kind: 'int', help: "Deployment id" },
      group_by: { kind: 'string!', help: "Comma-separated label names and/or attr:<name>" },
      metric: { kind: 'string!', help: "count | count_distinct:<k> | avg:<k> | p50:<k> | p95:<k> | p99:<k> | max:<k> | sum:<k>" },
      limit: { kind: 'int', help: "Max rows returned (default 50, cap 1000)" },
      attr: { kind: 'strings!', help: "Repeatable attribute predicate: <key><op><value> or <key>?" },
    },
  })

  endpoint(group, globalLogAttributeKeys, {
    name: "global-attribute-keys",
    description: "Global log attribute keys",
    query: {
      start_time: { kind: 'string!', help: "Window start (RFC 3339)" },
      end_time: { kind: 'string!', help: "Window end (RFC 3339)" },
      source: { kind: 'string', help: "Source kind (default: all)", values: ["collected","application","service"] },
      projects: { kind: 'strings', help: "Project selectors (repeatable)" },
      external_services: { kind: 'strings', help: "External service selectors (repeatable)" },
      scopes: { kind: 'strings', help: "Scope selectors (repeatable)" },
      levels: { kind: 'strings', help: "Levels (repeatable)", values: ["TRACE","DEBUG","INFO","WARN","ERROR"] },
      envs: { kind: 'strings', help: "Environments (repeatable)" },
      services: { kind: 'strings', help: "Services (repeatable)" },
      container_ids: { kind: 'strings', help: "Container ids (repeatable)" },
      node_ids: { kind: 'ints', help: "Node ids (repeatable)" },
      deploy_id: { kind: 'int', help: "Deployment id" },
      limit: { kind: 'int', help: "Max keys returned (default/cap 1000)" },
    },
  })

  endpoint(group, globalLogCapabilities, {
    name: "global-capabilities",
    description: "Global log capabilities",
  })

  endpoint(group, globalLogFacetsAttrs, {
    name: "global-facets-attrs",
    description: "Global log facets attrs",
    query: {
      start_time: { kind: 'string!', help: "Window start (RFC 3339)" },
      end_time: { kind: 'string!', help: "Window end (RFC 3339)" },
      source: { kind: 'string', help: "Source kind (default: all)", values: ["collected","application","service"] },
      projects: { kind: 'strings', help: "Project selectors (repeatable)" },
      external_services: { kind: 'strings', help: "External service selectors (repeatable)" },
      scopes: { kind: 'strings', help: "Scope selectors (repeatable)" },
      levels: { kind: 'strings', help: "Levels (repeatable)", values: ["TRACE","DEBUG","INFO","WARN","ERROR"] },
      envs: { kind: 'strings', help: "Environments (repeatable)" },
      services: { kind: 'strings', help: "Services (repeatable)" },
      container_ids: { kind: 'strings', help: "Container ids (repeatable)" },
      node_ids: { kind: 'ints', help: "Node ids (repeatable)" },
      deploy_id: { kind: 'int', help: "Deployment id" },
      keys: { kind: 'string!', help: "Comma-separated label names or attr:<name>" },
      attr: { kind: 'strings!', help: "Repeatable attribute predicate: <key><op><value> or <key>?" },
      limit: { kind: 'int', help: "Max values per key (default/cap 1000)" },
    },
  })

  endpoint(group, globalLogHistogram, {
    name: "global-histogram",
    description: "Global log histogram",
    query: {
      start_time: { kind: 'string!', help: "Window start (RFC 3339)" },
      end_time: { kind: 'string!', help: "Window end (RFC 3339)" },
      source: { kind: 'string', help: "Source kind (default: all)", values: ["collected","application","service"] },
      projects: { kind: 'strings', help: "Project selectors (repeatable)" },
      external_services: { kind: 'strings', help: "External service selectors (repeatable)" },
      scopes: { kind: 'strings', help: "Scope selectors (repeatable)" },
      levels: { kind: 'strings', help: "Levels (repeatable)", values: ["TRACE","DEBUG","INFO","WARN","ERROR"] },
      envs: { kind: 'strings', help: "Environments (repeatable)" },
      services: { kind: 'strings', help: "Services (repeatable)" },
      container_ids: { kind: 'strings', help: "Container ids (repeatable)" },
      node_ids: { kind: 'ints', help: "Node ids (repeatable)" },
      deploy_id: { kind: 'int', help: "Deployment id" },
      bucket_secs: { kind: 'int', help: "Bucket width in seconds (default 60)" },
      group_by: { kind: 'string', help: "One label name or attr:<name> to split series by" },
      max_groups: { kind: 'int', help: "Max series before folding the rest into \"other\" (default 8)" },
      attr: { kind: 'strings!', help: "Repeatable attribute predicate: <key><op><value> or <key>?" },
    },
  })

  endpoint(group, ingestLogs, {
    name: "ingest",
    description: "Ingest newline-delimited JSON log lines with a log ingest key",
    body: '@raw',
  })

  endpoint(group, logIngestBudget, {
    name: "ingest-budget",
    description: "The ingestion budget and the containers dropping lines over it, for the warnings on the log views",
  })

  endpoint(group, tailLogs, {
    name: "tail",
    description: "Live tail logs via Server-Sent Events",
    query: {
      project_id: { kind: 'string!', help: "Project ID" },
      service: { kind: 'string!', help: "Service name" },
      env: { kind: 'string!', help: "Environment" },
      levels: { kind: 'strings!', help: "Optional level filters" },
      text: { kind: 'string', help: "Optional text filter" },
    },
  })

  endpoint(group, purgeProjectLogs, {
    name: "purge",
    description: "Purge all logs for a project before a given timestamp",
    path: { project_id: 'project' },
    body: {
      before: { kind: 'string!', help: "Delete all logs before this timestamp (ISO 8601)" },
    },
    confirm: "This will purge all logs for a project before a given timestamp.",
  })

  endpoint(group, getSavedSearch, {
    name: "get-saved-search",
    description: "Read one saved log search",
    path: { project_id: 'project', saved_search_id: 'int' },
  })

  endpoint(group, updateSavedSearch, {
    name: "update-saved-search",
    description: "Change a saved log search",
    path: { project_id: 'project', saved_search_id: 'int' },
    body: {
      description: { kind: 'string', help: "Absent leaves the description unchanged; an empty string clears it" },
      name: { kind: 'string', help: "Absent leaves the name unchanged" },
      pinned: 'bool',
      query: { kind: 'json', help: "The stored filter set of a saved search" },
      time_range: 'string',
    },
  })
}
