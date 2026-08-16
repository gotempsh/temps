import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  checkExplorerSupport,
  listRootContainers,
  listContainersAtPath,
  listEntities,
  getEntityInfo,
  readEntityRows,
  getAiDataAccess,
  setAiDataAccess,
  listServices,
} from '../../api/sdk.gen.js'
import type {
  ContainerResponse,
  EntityResponse,
  FieldResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { sanitizeTerminalText } from '../../ui/terminal.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
  keyValue,
} from '../../ui/output.js'

/**
 * Read-only browsing of the data *inside* a managed service — the tables,
 * collections, keys and objects, not the service's own configuration.
 *
 * Exists primarily so coding agents (Claude Code, Codex, OpenCode) can answer
 * questions about an application's real data without a human driving the
 * console. Every command here is a read; nothing in this group mutates
 * application data. The only writer is `data ai-access`, which toggles whether
 * the *built-in* assistant may read rows — see the note on that command.
 */
export function registerDataCommands(program: Command): void {
  const data = program
    .command('data')
    .description(
      'Browse the data inside a service (tables, collections, keys, objects) — read-only',
    )

  data
    .command('info <service>')
    .description('Show what a service supports and how its containers nest')
    .option('--json', 'Output in JSON format')
    .action(infoCmd)

  data
    .command('containers <service>')
    .alias('databases')
    .description('List top-level containers (databases, or buckets for S3)')
    .option('--path <path>', 'List containers nested under this path instead of the root')
    .option('--json', 'Output in JSON format')
    .action(containersCmd)

  data
    .command('tables <service>')
    .alias('entities')
    .description('List tables, collections, keys or objects in a container')
    .requiredOption('--path <path>', 'Container path, slash-separated (e.g. mydb/public)')
    .option('--limit <n>', 'Maximum entities to return (default: 100)')
    .option('--json', 'Output in JSON format')
    .action(tablesCmd)

  data
    .command('schema <service> <entity>')
    .alias('columns')
    .description('Show an entity\'s columns, types and row count')
    .requiredOption('--path <path>', 'Container path, slash-separated (e.g. mydb/public)')
    .option('--json', 'Output in JSON format')
    .action(schemaCmd)

  data
    .command('rows <service> <entity>')
    .alias('select')
    .description('Read rows from an entity')
    .requiredOption('--path <path>', 'Container path, slash-separated (e.g. mydb/public)')
    .option(
      '--filter <json>',
      'Backend-specific filter as JSON (SQL: \'{"where":"id > 5"}\'). See: temps data info <service>',
    )
    .option('--limit <n>', 'Maximum rows to return (default: 20)')
    .option('--offset <n>', 'Rows to skip (default: 0)')
    .option('--sort-by <field>', 'Field to sort by')
    .option('--sort-order <order>', 'asc or desc (default: asc)')
    .option('--json', 'Output in JSON format')
    .action(rowsCmd)

  data
    .command('ai-access <service>')
    .description(
      'Show or set whether the built-in AI assistant may read this service\'s rows',
    )
    .option('--enable', 'Allow the built-in assistant to read row data')
    .option('--disable', 'Stop the built-in assistant reading row data')
    .option('--json', 'Output in JSON format')
    .action(aiAccessCmd)
}

// ============================================================================
// Helpers
// ============================================================================

/**
 * Resolve a service by numeric id, slug or name.
 *
 * Agents and humans both refer to a service the way they think of it ("the
 * landing production db"), not by id, so accept all three rather than forcing
 * a lookup step first.
 */
async function resolveService(
  nameOrId: string,
): Promise<{ id: number; name: string; service_type: string }> {
  const asId = Number(nameOrId)
  const { data, error } = await listServices({ client })
  if (error) throw new Error(getErrorMessage(error))

  const services = data ?? []
  const match = Number.isInteger(asId)
    ? services.find((s) => s.id === asId)
    : services.find(
        (s) => s.name === nameOrId || s.name.toLowerCase() === nameOrId.toLowerCase(),
      )

  if (!match) {
    const available = services.map((s) => sanitizeTerminalText(s.name)).join(', ')
    throw new Error(
      `Service "${sanitizeTerminalText(nameOrId)}" not found. Available: ${available || '(none)'}`,
    )
  }
  return { id: match.id, name: match.name, service_type: match.service_type }
}

/**
 * Render a cell for tabular output.
 *
 * Row values are arbitrary user data, so they need clamping — a single JSON
 * blob or a long text column otherwise destroys the table. `--json` is the
 * escape hatch and returns everything untouched.
 */
export function cell(value: unknown, maxLength = 40): string {
  if (value === null || value === undefined) return colors.dim('null')
  const raw = typeof value === 'string' ? value : JSON.stringify(value)
  const text = sanitizeTerminalText(raw)
  return text.length > maxLength ? `${text.slice(0, maxLength - 1)}…` : text
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return colors.dim('—')
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit++
  }
  return `${Math.round(value * 10) / 10} ${units[unit]}`
}

/**
 * Turn a `filter` string into the request value, failing loudly on bad JSON.
 *
 * Passing an unparsable filter through would let the server drop it and
 * return the whole unfiltered table, which reads as a correct answer. Better
 * to refuse before the request leaves.
 */
export function validateFilter(raw: string | undefined, service: string): string | undefined {
  if (raw === undefined) return undefined
  try {
    JSON.parse(raw)
    return raw
  } catch {
    throw new Error(
      `--filter must be valid JSON. Received: ${sanitizeTerminalText(raw)}. ` +
        `Run "temps data info ${sanitizeTerminalText(service)}" to see this backend's filter schema.`,
    )
  }
}

// ============================================================================
// Commands
// ============================================================================

async function infoCmd(
  serviceRef: string,
  options: { json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const service = await resolveService(serviceRef)

  const support = await withSpinner('Checking data browser support...', async () => {
    const { data, error } = await checkExplorerSupport({
      client,
      path: { service_id: service.id },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(support ?? {})
    return
  }

  newline()
  header(
    `${icons.info} ${sanitizeTerminalText(service.name)} (${sanitizeTerminalText(service.service_type)})`,
  )
  newline()

  if (!support?.supported) {
    warning('This service does not support data browsing.')
    if (support?.reason) info(support.reason)
    newline()
    return
  }

  keyValue('Capabilities', (support.capabilities ?? []).join(', ') || '—')
  newline()

  info('Container hierarchy:')
  for (const level of support.hierarchy ?? []) {
    const holds = level.can_list_entities
      ? 'holds tables/objects'
      : level.can_list_containers
        ? 'holds containers'
        : 'leaf'
    console.log(
      `  ${colors.muted(`level ${level.level}`)} ${colors.bold(sanitizeTerminalText(level.container_type))} ${colors.dim(`— ${holds}`)}`,
    )
  }
  newline()

  if (support.filter_schema) {
    info('Filter schema (pass to --filter as JSON):')
    console.log(colors.dim(JSON.stringify(support.filter_schema, null, 2)))
    newline()
  }

  info(`Next: temps data containers ${sanitizeTerminalText(service.name)}`)
  newline()
}

async function containersCmd(
  serviceRef: string,
  options: { path?: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const service = await resolveService(serviceRef)

  const containers = await withSpinner('Fetching containers...', async () => {
    if (options.path) {
      const { data, error } = await listContainersAtPath({
        client,
        path: { service_id: service.id, path: options.path },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    }
    const { data, error } = await listRootContainers({
      client,
      path: { service_id: service.id },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const rows = containers ?? []

  if (options.json) {
    json(rows)
    return
  }

  const scope = options.path ? ` under ${sanitizeTerminalText(options.path)}` : ''
  newline()
  header(
    `${icons.folder} Containers in ${sanitizeTerminalText(service.name)}${scope} (${rows.length})`,
  )

  if (rows.length === 0) {
    info('No containers found.')
    newline()
    return
  }

  const columns: TableColumn<ContainerResponse>[] = [
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'container_type' },
    {
      header: 'Holds',
      accessor: (c) =>
        c.can_contain_entities
          ? (c.entity_type_label ?? 'entities')
          : c.can_contain_containers
            ? (c.child_container_type ?? 'containers')
            : '—',
      color: (value, c) =>
        c.can_contain_entities || c.can_contain_containers ? value : colors.dim(value),
    },
  ]
  printTable(rows, columns, { style: 'minimal' })

  // Tell the user what to run next — the path syntax is the single most
  // common thing to get wrong, so show a real one rather than a placeholder.
  const first = rows[0]
  if (first) {
    const nextPath = sanitizeTerminalText(
      options.path ? `${options.path}/${first.name}` : first.name,
    )
    const safeServiceName = sanitizeTerminalText(service.name)
    if (first.can_contain_entities) {
      info(`Next: temps data tables ${safeServiceName} --path ${nextPath}`)
    } else if (first.can_contain_containers) {
      info(`Next: temps data containers ${safeServiceName} --path ${nextPath}`)
    }
  }
  newline()
}

async function tablesCmd(
  serviceRef: string,
  options: { path: string; limit?: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const service = await resolveService(serviceRef)

  const result = await withSpinner('Fetching entities...', async () => {
    const { data, error } = await listEntities({
      client,
      path: { service_id: service.id, path: options.path },
      query: options.limit ? { limit: Number(options.limit) } : undefined,
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const entities = result?.entities ?? []

  if (options.json) {
    json(result ?? { entities: [], count: 0 })
    return
  }

  newline()
  header(
    `${icons.folder} ${sanitizeTerminalText(options.path)} in ${sanitizeTerminalText(service.name)} (${entities.length})`,
  )

  if (entities.length === 0) {
    info(
      'No entities found. Check the path with: temps data containers ' +
        sanitizeTerminalText(service.name),
    )
    newline()
    return
  }

  const columns: TableColumn<EntityResponse>[] = [
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'entity_type' },
    {
      header: 'Rows',
      accessor: (e) => (e.row_count === null || e.row_count === undefined ? '—' : String(e.row_count)),
      color: (value, e) => e.row_count === null || e.row_count === undefined ? colors.dim(value) : value,
    },
    { header: 'Size', accessor: (e) => formatBytes(e.size_bytes) },
  ]
  printTable(entities, columns, { style: 'minimal' })

  if (result?.has_more) {
    info('More entities available — raise --limit to see them.')
  }
  const firstEntity = entities[0]
  if (firstEntity) {
    info(
      `Next: temps data rows ${sanitizeTerminalText(service.name)} ${sanitizeTerminalText(firstEntity.name)} --path ${sanitizeTerminalText(options.path)}`,
    )
  }
  newline()
}

async function schemaCmd(
  serviceRef: string,
  entity: string,
  options: { path: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const service = await resolveService(serviceRef)

  const info_ = await withSpinner('Fetching schema...', async () => {
    const { data, error } = await getEntityInfo({
      client,
      path: { service_id: service.id, path: options.path, entity },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(info_ ?? {})
    return
  }

  newline()
  header(
    `${icons.info} ${sanitizeTerminalText(options.path)}/${sanitizeTerminalText(entity)} (${sanitizeTerminalText(info_?.entity_type ?? 'entity')})`,
  )
  newline()
  if (info_?.row_count !== null && info_?.row_count !== undefined) {
    keyValue('Rows', String(info_.row_count))
  }
  if (info_?.size_bytes !== null && info_?.size_bytes !== undefined) {
    keyValue('Size', formatBytes(info_.size_bytes))
  }
  newline()

  const fields = info_?.fields ?? []
  if (fields.length === 0) {
    info('This entity exposes no column schema (typical for key-value and object stores).')
    newline()
    return
  }

  const columns: TableColumn<FieldResponse>[] = [
    { header: 'Column', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'field_type' },
    { header: 'Nullable', accessor: (f) => (f.nullable ? 'yes' : 'no') },
  ]
  printTable(fields, columns, { style: 'minimal' })
  newline()
}

async function rowsCmd(
  serviceRef: string,
  entity: string,
  options: {
    path: string
    filter?: string
    limit?: string
    offset?: string
    sortBy?: string
    sortOrder?: string
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const service = await resolveService(serviceRef)
  const filter = validateFilter(options.filter, service.name)

  const result = await withSpinner('Reading rows...', async () => {
    const { data, error } = await readEntityRows({
      client,
      path: { service_id: service.id, path: options.path, entity },
      query: {
        ...(filter ? { filter } : {}),
        limit: options.limit ? Number(options.limit) : 20,
        ...(options.offset ? { offset: Number(options.offset) } : {}),
        ...(options.sortBy ? { sort_by: options.sortBy } : {}),
        ...(options.sortOrder ? { sort_order: options.sortOrder } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(result ?? { fields: [], rows: [], total_count: 0 })
    return
  }

  const rows = (result?.rows ?? []) as Array<Record<string, unknown>>
  const fields = result?.fields ?? []

  newline()
  header(
    `${icons.info} ${sanitizeTerminalText(options.path)}/${sanitizeTerminalText(entity)} — ${result?.returned_count ?? rows.length} of ${result?.total_count ?? '?'} rows`,
  )

  if (rows.length === 0) {
    info('No rows matched.')
    newline()
    return
  }

  const columns: TableColumn<Record<string, unknown>>[] = fields.map((f) => ({
    header: f.name,
    accessor: (row) => cell(row[f.name]),
  }))
  printTable(rows, columns, { style: 'minimal' })

  const offset = options.offset ? Number(options.offset) : 0
  const shown = offset + rows.length

  // The server drops rows to stay inside a response byte budget when a table
  // holds large values (blobs, big JSON). Say so explicitly: without this the
  // short page is indistinguishable from having reached the end of the table,
  // and a script paging on `rows.length < limit` would stop early and silently
  // miss data.
  if (result?.truncated) {
    warning(
      `Response was truncated to stay within the size limit — ${rows.length} row(s) shown, and there are more at this offset.`,
    )
    warning(`Narrow the columns with a filter, or continue with --offset ${shown}.`)
  }

  if (result?.total_count !== undefined && shown < result.total_count) {
    info(`Showing ${offset + 1}–${shown} of ${result.total_count}. Next: --offset ${shown}`)
  }
  info('Values are truncated for display — use --json for full values.')
  newline()
}

async function aiAccessCmd(
  serviceRef: string,
  options: { enable?: boolean; disable?: boolean; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (options.enable && options.disable) {
    throw new Error('Pass either --enable or --disable, not both.')
  }

  const service = await resolveService(serviceRef)

  // No flag = report current state. This command is the discoverable surface
  // for a setting that is otherwise invisible, so reading must not require
  // guessing which flag is "safe" to pass.
  if (!options.enable && !options.disable) {
    const current = await withSpinner('Fetching setting...', async () => {
      const { data, error } = await getAiDataAccess({
        client,
        path: { service_id: service.id },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    })

    if (options.json) {
      json(current ?? {})
      return
    }

    newline()
    header(`${icons.info} AI data access — ${sanitizeTerminalText(service.name)}`)
    newline()
    keyValue(
      'Built-in assistant may read rows',
      current?.enabled ? colors.warning('yes') : 'no',
    )
    newline()
    info(
      current?.enabled
        ? `Disable with: temps data ai-access ${sanitizeTerminalText(service.name)} --disable`
        : `Enable with: temps data ai-access ${sanitizeTerminalText(service.name)} --enable`,
    )
    info(
      colors.dim(
        'This gates only the built-in AI assistant. It does not restrict this CLI,',
      ),
    )
    info(colors.dim('the console, or any API key — those are governed by permissions.'))
    newline()
    return
  }

  const enabled = Boolean(options.enable)

  const updated = await withSpinner(
    enabled ? 'Enabling AI data access...' : 'Disabling AI data access...',
    async () => {
      const { data, error } = await setAiDataAccess({
        client,
        path: { service_id: service.id },
        body: { enabled },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    },
  )

  if (options.json) {
    json(updated ?? {})
    return
  }

  newline()
  if (enabled) {
    success(
      `The built-in AI assistant can now read rows from ${sanitizeTerminalText(service.name)}.`,
    )
    newline()
    warning(
      'Rows are sent to your configured AI provider. If this service stores password',
    )
    warning('hashes, tokens or personal data, that data leaves your infrastructure.')
  } else {
    success(
      `The built-in AI assistant can no longer read rows from ${sanitizeTerminalText(service.name)}.`,
    )
    info('Table and column names remain readable.')
  }
  newline()
}
