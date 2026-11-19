// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { resolveProjectRef } from '../delivery/index.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import {
  colors,
  header,
  info,
  json,
  keyValue,
  newline,
  success,
  warning,
} from '../../ui/output.js'
import {
  createLogIngestKey,
  createSavedSearch,
  deleteSavedSearch,
  listLogIngestKeys,
  listSavedSearches,
  revokeLogIngestKey,
  savedSearchSeries,
} from '../../api/sdk.gen.js'
import type {
  LogIngestKeyResponse,
  LogLevel,
  LogSourceKind,
  SavedSearchQuery,
  SavedSearchResponse,
} from '../../api/types.gen.js'
import { parseDuration, validateAttrPredicate } from './index.js'

const LEVELS: LogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']
const SOURCES: LogSourceKind[] = ['collected', 'application', 'service']
const TIME_RANGES = ['15m', '1h', '6h', '24h', '7d', '30d']

const collect = (value: string, previous: string[]): string[] => [
  ...previous,
  value,
]

export interface SavedQueryOptions {
  source?: string
  level?: string[]
  env?: string[]
  service?: string[]
  container?: string[]
  node?: string[]
  deploy?: string
  text?: string
  attr?: string[]
}

function positiveInt(value: string, label: string): number {
  if (!/^\d+$/.test(value) || parseInt(value, 10) < 1)
    throw new Error(`${label} must be a positive integer (got "${value}")`)
  return parseInt(value, 10)
}

/** The filter flags of `logs saved create`, as a saved search stores them. */
export function buildSavedQuery(options: SavedQueryOptions): SavedSearchQuery {
  const source = (options.source ?? 'collected') as LogSourceKind
  if (!SOURCES.includes(source))
    throw new Error(
      `Invalid --source "${options.source}". Use ${SOURCES.join(', ')}.`,
    )
  const levels = (options.level ?? []).map((level) => {
    const upper = level.toUpperCase() as LogLevel
    if (!LEVELS.includes(upper))
      throw new Error(`Invalid --level "${level}". Use ${LEVELS.join(', ')}.`)
    return upper
  })
  const text = options.text?.trim()
  return {
    source,
    levels,
    envs: options.env ?? [],
    services: options.service ?? [],
    container_ids: options.container ?? [],
    node_ids: (options.node ?? []).map((n) => positiveInt(n, '--node')),
    external_service_ids: [],
    deploy_id:
      options.deploy !== undefined
        ? positiveInt(options.deploy, '--deploy')
        : null,
    text: text || null,
    attrs: (options.attr ?? []).map(validateAttrPredicate),
  }
}

/** `count`, or `<fn>:<attribute>` for a numeric field. */
export function validateSeriesMetric(raw: string): string {
  if (raw === 'count') return raw
  const match = /^(avg|sum|max|p50|p95|p99|count_distinct):([A-Za-z0-9_.:-]{1,200})$/.exec(
    raw,
  )
  if (!match)
    throw new Error(
      `Invalid --metric "${raw}". Use count, or avg|sum|max|p50|p95|p99|count_distinct:<attribute>.`,
    )
  return raw
}

async function projectId(flag?: string): Promise<number> {
  const resolved = await requireProjectSlug(flag)
  if (resolved.source !== 'flag')
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  return resolveProjectRef(resolved.slug)
}

async function connect(): Promise<void> {
  await requireAuth()
  await setupClient()
}

function describeQuery(query: SavedSearchQuery): string {
  const parts: string[] = []
  if (query.source && query.source !== 'collected')
    parts.push(`source:${query.source}`)
  if (query.levels?.length) parts.push(`level:${query.levels.join(',')}`)
  if (query.envs?.length) parts.push(`env:${query.envs.join(',')}`)
  if (query.services?.length) parts.push(`service:${query.services.join(',')}`)
  if (query.deploy_id != null) parts.push(`deploy:${query.deploy_id}`)
  for (const attr of query.attrs ?? []) parts.push(attr)
  if (query.text) parts.push(`"${query.text}"`)
  return parts.join(' ') || '(all lines)'
}

export function registerSavedLogCommands(logs: Command): void {
  const saved = logs
    .command('saved')
    .description(
      'Saved log searches: named filters you can reopen, chart on a dashboard and alert on',
    )

  saved
    .command('list')
    .description('List the saved searches of a project, pinned first')
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .option('--json', 'Output in JSON format')
    .action(async (options: { project?: string; json?: boolean }) => {
      await connect()
      const project_id = await projectId(options.project)
      const { data, error } = await withSpinner('Loading saved searches...', () =>
        listSavedSearches({ client, path: { project_id }, query: { page_size: 100 } }),
      )
      if (error || !data)
        throw new Error(getErrorMessage(error) || 'Failed to list saved searches')
      if (options.json) return json(data.data)
      newline()
      header('Saved log searches')
      printTable<SavedSearchResponse>(data.data, [
        { header: 'ID', key: 'id' },
        { header: 'Name', accessor: (s) => (s.pinned ? `★ ${s.name}` : s.name) },
        { header: 'Filters', accessor: (s) => describeQuery(s.query) },
        { header: 'Range', key: 'time_range' },
        { header: 'Alerts', key: 'alert_count', align: 'right' },
      ])
    })

  saved
    .command('create')
    .description('Save a log search')
    .requiredOption('--name <name>', 'Name, unique within the project')
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .option('--description <text>', 'What the search is for')
    .option('--source <kind>', 'collected (default), application, or service')
    .option('--level <level>', 'Log level, repeatable', collect, [])
    .option('--env <name>', 'Environment, repeatable', collect, [])
    .option('--service <name>', 'Service label, repeatable', collect, [])
    .option('--container <id>', 'Container id, repeatable', collect, [])
    .option('--node <id>', 'Node id, repeatable', collect, [])
    .option('--deploy <id>', 'Deployment id')
    .option('--text <needle>', 'Text the message must contain')
    .option(
      '--attr <predicate>',
      'Attribute predicate, repeatable: key=value, key!=value, key^=prefix, key>n, key<n, key?',
      collect,
      [],
    )
    .option(
      '--time-range <range>',
      `Range the search opens with: ${TIME_RANGES.join(', ')} (default 1h)`,
    )
    .option('--pinned', 'Pin it to the top of the list')
    .option('--json', 'Output in JSON format')
    .action(
      async (
        options: SavedQueryOptions & {
          name: string
          project?: string
          description?: string
          timeRange?: string
          pinned?: boolean
          json?: boolean
        },
      ) => {
        const query = buildSavedQuery(options)
        if (options.timeRange && !TIME_RANGES.includes(options.timeRange))
          throw new Error(
            `Invalid --time-range "${options.timeRange}". Use ${TIME_RANGES.join(', ')}.`,
          )
        await connect()
        const project_id = await projectId(options.project)
        const { data, error } = await withSpinner('Saving search...', () =>
          createSavedSearch({
            client,
            path: { project_id },
            body: {
              name: options.name,
              description: options.description ?? null,
              query,
              time_range: options.timeRange ?? null,
              pinned: options.pinned ?? false,
            },
          }),
        )
        if (error || !data)
          throw new Error(getErrorMessage(error) || 'Failed to save the search')
        if (options.json) return json(data)
        success(`Saved "${data.name}" (id ${data.id})`)
      },
    )

  saved
    .command('delete <id>')
    .description(
      'Delete a saved search (refused while an alert rule counts it)',
    )
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .action(async (id: string, options: { project?: string }) => {
      const saved_search_id = positiveInt(id, 'Saved search id')
      await connect()
      const project_id = await projectId(options.project)
      const { error } = await withSpinner('Deleting saved search...', () =>
        deleteSavedSearch({ client, path: { project_id, saved_search_id } }),
      )
      if (error)
        throw new Error(getErrorMessage(error) || `Failed to delete saved search ${id}`)
      success(`Deleted saved search ${id}`)
    })

  saved
    .command('series <id>')
    .description(
      'Chart a saved search over time: matching lines per bucket, or a numeric field',
    )
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .option('--since <duration>', 'Window ending now, e.g. 30m, 6h, 1d (default 1h)')
    .option('--bucket-secs <n>', 'Bucket width in seconds (default 60)')
    .option(
      '--metric <spec>',
      'count (default), or avg|sum|max|p50|p95|p99|count_distinct:<attribute>',
    )
    .option('--json', 'Output in JSON format')
    .action(
      async (
        id: string,
        options: {
          project?: string
          since?: string
          bucketSecs?: string
          metric?: string
          json?: boolean
        },
      ) => {
        const saved_search_id = positiveInt(id, 'Saved search id')
        const metric = validateSeriesMetric(options.metric ?? 'count')
        const end = new Date()
        const start = new Date(end.getTime() - parseDuration(options.since ?? '1h'))
        const bucket_secs =
          options.bucketSecs !== undefined
            ? positiveInt(options.bucketSecs, '--bucket-secs')
            : undefined
        await connect()
        const project_id = await projectId(options.project)
        const { data, error } = await withSpinner('Counting...', () =>
          savedSearchSeries({
            client,
            path: { project_id, saved_search_id },
            query: {
              start_time: start.toISOString(),
              end_time: end.toISOString(),
              bucket_secs,
              metric,
            },
          }),
        )
        if (error || !data)
          throw new Error(getErrorMessage(error) || `Failed to chart saved search ${id}`)
        if (options.json) return json(data)
        newline()
        printTable(data.points, [
          { header: 'Bucket', accessor: (p) => p.ts },
          {
            header: metric,
            accessor: (p) => (p.value == null ? '—' : p.value),
            align: 'right',
          },
        ])
        if (data.partial_before)
          warning(
            `Counted by a capped scan: buckets before ${data.partial_before} have no value.`,
          )
      },
    )
}

export function registerLogIngestKeyCommands(logs: Command): void {
  const keys = logs
    .command('keys')
    .description(
      'Ingest keys for POST /api/logs/ingest (newline-delimited JSON from Vector, Fluent Bit or any HTTP client)',
    )

  keys
    .command('list')
    .description('List the ingest keys of a project')
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .option('--json', 'Output in JSON format')
    .action(async (options: { project?: string; json?: boolean }) => {
      await connect()
      const project_id = await projectId(options.project)
      const { data, error } = await withSpinner('Loading ingest keys...', () =>
        listLogIngestKeys({ client, path: { project_id } }),
      )
      if (error || !data)
        throw new Error(getErrorMessage(error) || 'Failed to list ingest keys')
      if (options.json) return json(data.data)
      newline()
      header('Log ingest keys')
      printTable<LogIngestKeyResponse>(data.data, [
        { header: 'ID', key: 'id' },
        { header: 'Name', key: 'name' },
        { header: 'Prefix', accessor: (k) => `${k.key_prefix}…` },
        { header: 'Service', key: 'default_service' },
        { header: 'Lines', key: 'line_count', align: 'right' },
        { header: 'Last used', accessor: (k) => k.last_used_at ?? 'never' },
        { header: 'State', accessor: (k) => (k.is_active ? 'active' : 'revoked') },
      ])
    })

  keys
    .command('create')
    .description('Create an ingest key; the key is printed once')
    .requiredOption('--name <name>', 'Name, e.g. the host or collector')
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .option('--service <name>', 'Service label for lines that name none')
    .option('--environment <id>', 'Pin lines to an environment id')
    .option('--json', 'Output in JSON format')
    .action(
      async (options: {
        name: string
        project?: string
        service?: string
        environment?: string
        json?: boolean
      }) => {
        const environment_id =
          options.environment !== undefined
            ? positiveInt(options.environment, '--environment')
            : null
        await connect()
        const project_id = await projectId(options.project)
        const { data, error } = await withSpinner('Creating ingest key...', () =>
          createLogIngestKey({
            client,
            path: { project_id },
            body: {
              name: options.name,
              default_service: options.service ?? null,
              environment_id,
            },
          }),
        )
        if (error || !data)
          throw new Error(getErrorMessage(error) || 'Failed to create the ingest key')
        if (options.json) return json(data)
        success(`Created "${data.name}" (id ${data.id})`)
        newline()
        keyValue('Key', data.key)
        warning('Copy the key now. It is not shown again.')
        newline()
        info(
          "Send lines with: curl -X POST <public-ingress-url>/api/logs/ingest -H 'Authorization: Bearer <key>' --data-binary @lines.ndjson",
        )
      },
    )

  keys
    .command('revoke <id>')
    .description('Revoke an ingest key; collectors using it are refused')
    .option('--project <slug|id>', 'Project (defaults to the linked project)')
    .action(async (id: string, options: { project?: string }) => {
      const key_id = positiveInt(id, 'Key id')
      await connect()
      const project_id = await projectId(options.project)
      const { error } = await withSpinner('Revoking ingest key...', () =>
        revokeLogIngestKey({ client, path: { project_id, key_id } }),
      )
      if (error)
        throw new Error(getErrorMessage(error) || `Failed to revoke ingest key ${id}`)
      success(`Revoked ingest key ${id}`)
    })
}
