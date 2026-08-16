import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listAuditLogs,
  getAuditLog,
} from '../../api/sdk.gen.js'
import type { AuditLogIpInfo, AuditLogResponse, AuditLogUserInfo } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'

interface ListOptions {
  json?: boolean
  limit?: string
  offset?: string
  operationType?: string
  userId?: string
  from?: string
  to?: string
}

interface ShowOptions {
  id: string
  json?: boolean
}

export function registerAuditCommands(program: Command): void {
  const audit = program
    .command('audit')
    .description('View audit logs')

  audit
    .command('list')
    .alias('ls')
    .description('List audit logs')
    .option('--json', 'Output in JSON format')
    .option('--limit <n>', 'Maximum number of logs to return', '50')
    .option('--offset <n>', 'Number of logs to skip')
    .option('--operation-type <type>', 'Filter by operation type')
    .option('--user-id <id>', 'Filter by user ID')
    .option('--from <timestamp>', 'Start timestamp (ISO 8601 or epoch ms)')
    .option('--to <timestamp>', 'End timestamp (ISO 8601 or epoch ms)')
    .action(listAuditLogsAction)

  audit
    .command('show')
    .description('Show audit log details')
    .requiredOption('--id <id>', 'Audit log ID')
    .option('--json', 'Output in JSON format')
    .action(showAuditLogAction)
}

// ============================================================================
// Helpers
// ============================================================================

/** Who performed the action: display name, falling back to email, then the raw ID. */
export function formatAuditUser(log: {
  user?: AuditLogUserInfo | null
  user_id?: number | null
}): string {
  return log.user ? log.user.name || log.user.email : `ID: ${log.user_id}`
}

/** City/country summary for the table view; `-` when nothing is known. */
export function formatIpLocation(ipAddress: AuditLogIpInfo | null | undefined): string {
  if (!ipAddress) return '-'
  const parts: string[] = []
  if (ipAddress.city) parts.push(ipAddress.city)
  if (ipAddress.country) parts.push(ipAddress.country)
  return parts.length > 0 ? parts.join(', ') : '-'
}

/** City/country joined for the detail view, which only prints the line when non-empty. */
export function joinLocationParts(ipAddress: AuditLogIpInfo | null | undefined): string {
  if (!ipAddress) return ''
  const parts: string[] = []
  if (ipAddress.city) parts.push(ipAddress.city)
  if (ipAddress.country) parts.push(ipAddress.country)
  return parts.join(', ')
}

/** Render the audit log's free-form `data` field, tolerating non-serializable values. */
export function formatAuditData(data: unknown): string {
  try {
    return typeof data === 'string' ? data : JSON.stringify(data, null, 2)
  } catch {
    return String(data)
  }
}

// ============================================================================
// Commands
// ============================================================================

async function listAuditLogsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const limit = parseInt(options.limit || '50', 10)
  const offset = options.offset ? parseInt(options.offset, 10) : 0
  const userId = options.userId ? parseInt(options.userId, 10) : undefined

  const logs = await withSpinner('Fetching audit logs...', async () => {
    const { data, error } = await listAuditLogs({
      client,
      query: {
        limit,
        offset,
        operation_type: options.operationType ?? undefined,
        user_id: userId ?? undefined,
        from: options.from ?? undefined,
        to: options.to ?? undefined,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(logs)
    return
  }

  newline()
  header(`${icons.info} Audit Logs (${logs.length})`)

  if (logs.length === 0) {
    info('No audit logs found')
    newline()
    return
  }

  const columns: TableColumn<AuditLogResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Operation', key: 'operation_type', color: (v) => colors.bold(v) },
    { header: 'User', accessor: formatAuditUser },
    { header: 'IP', accessor: (l) => l.ip_address ? l.ip_address.ip : '-', color: (v) => colors.muted(v) },
    { header: 'Location', accessor: (l) => formatIpLocation(l.ip_address), color: (v) => colors.muted(v) },
    { header: 'Date', accessor: (l) => formatRelativeTime(new Date(l.audit_date)) },
  ]

  printTable(logs, columns, { style: 'minimal' })
  newline()
}

async function showAuditLogAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid audit log ID')
    return
  }

  const log = await withSpinner('Fetching audit log...', async () => {
    const { data, error } = await getAuditLog({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Audit log ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(log)
    return
  }

  newline()
  header(`${icons.info} Audit Log #${log.id}`)
  keyValue('Operation', colors.bold(log.operation_type))
  keyValue('User ID', log.user_id)

  if (log.user) {
    keyValue('User Name', log.user.name)
    keyValue('User Email', log.user.email)
  }

  if (log.ip_address) {
    keyValue('IP Address', log.ip_address.ip)
    if (log.ip_address.city || log.ip_address.country) {
      keyValue('Location', joinLocationParts(log.ip_address))
    }
    if (log.ip_address.latitude != null && log.ip_address.longitude != null) {
      keyValue('Coordinates', `${log.ip_address.latitude}, ${log.ip_address.longitude}`)
    }
  }

  keyValue('Date', new Date(log.audit_date).toLocaleString())

  if (log.data) {
    newline()
    header('Additional Data')
    console.log(formatAuditData(log.data))
  }
  newline()
}
