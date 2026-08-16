import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listIpAccessControl,
  createIpAccessControl,
  getIpAccessControl,
  updateIpAccessControl,
  deleteIpAccessControl,
  checkIpBlocked,
} from '../../api/sdk.gen.js'
import type { IpAccessControlResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'

interface ListOptions {
  json?: boolean
}

interface CreateOptions {
  ip?: string
  action?: string
  description?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface UpdateOptions {
  id: string
  ip?: string
  action?: string
  description?: string
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface CheckOptions {
  ip?: string
  json?: boolean
}

const IP_ACCESS_ACTIONS = ['allow', 'deny', 'block']

/**
 * Validates a raw --action value and normalizes "deny" to the "block" value
 * the API expects. Returns undefined for anything not in the allowed set so
 * callers can reject before a malformed action reaches the API.
 */
export function resolveIpAccessAction(rawAction: string): string | undefined {
  if (!IP_ACCESS_ACTIONS.includes(rawAction)) {
    return undefined
  }
  return rawAction === 'deny' ? 'block' : rawAction
}

export function registerIpAccessCommands(program: Command): void {
  const ipAccess = program
    .command('ip-access')
    .alias('ipa')
    .description('Manage IP access control rules')

  ipAccess
    .command('list')
    .alias('ls')
    .description('List all IP access control rules')
    .option('--json', 'Output in JSON format')
    .action(listIpAccessAction)

  ipAccess
    .command('create')
    .alias('add')
    .description('Create a new IP access control rule')
    .option('--ip <ip_or_cidr>', 'IP address or CIDR range (e.g., "192.168.1.1" or "10.0.0.0/24")')
    .option('--action <action>', 'Action to take: "allow" or "deny"')
    .option('--description <desc>', 'Optional description/reason for the rule')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createIpAccessAction)

  ipAccess
    .command('show')
    .description('Show IP access control rule details')
    .requiredOption('--id <id>', 'Rule ID')
    .option('--json', 'Output in JSON format')
    .action(showIpAccessAction)

  ipAccess
    .command('update')
    .description('Update an IP access control rule')
    .requiredOption('--id <id>', 'Rule ID')
    .option('--ip <ip>', 'New IP address or CIDR range')
    .option('--action <action>', 'New action: "allow" or "deny"')
    .option('--description <desc>', 'New description/reason')
    .action(updateIpAccessAction)

  ipAccess
    .command('remove')
    .alias('rm')
    .description('Delete an IP access control rule')
    .requiredOption('--id <id>', 'Rule ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeIpAccessAction)

  ipAccess
    .command('check')
    .description('Check if an IP address is blocked')
    .option('--ip <ip>', 'IP address to check')
    .option('--json', 'Output in JSON format')
    .action(checkIpAction)
}

async function listIpAccessAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const rules = await withSpinner('Fetching IP access control rules...', async () => {
    const { data, error } = await listIpAccessControl({
      client,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(rules)
    return
  }

  newline()
  header(`${icons.info} IP Access Control Rules (${rules.length})`)

  if (rules.length === 0) {
    info('No IP access control rules configured')
    info('Run: temps ip-access create --ip 192.168.1.0/24 --action deny --description "Block subnet" -y')
    newline()
    return
  }

  const columns: TableColumn<IpAccessControlResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'IP Address', key: 'ip_address', color: (v) => colors.bold(v) },
    { header: 'Action', key: 'action', color: (v) => v === 'allow' ? statusBadge('active') : statusBadge('error') },
    { header: 'Reason', accessor: (r) => r.reason || '-', color: (v) => colors.muted(v) },
    { header: 'Created', accessor: (r) => formatRelativeTime(r.created_at) },
  ]

  printTable(rules, columns, { style: 'minimal' })
  newline()
}

async function createIpAccessAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let ipAddress: string
  let action: string
  let reason: string | null = null

  const isAutomation = options.yes && options.ip && options.action

  if (isAutomation) {
    ipAddress = options.ip!
    action = options.action!
    reason = options.description || null

    const resolved = resolveIpAccessAction(action)
    if (resolved === undefined) {
      warning(`Invalid action: ${action}. Available: allow, deny`)
      return
    }
    action = resolved
  } else {
    ipAddress = options.ip || await promptText({
      message: 'IP address or CIDR range',
      required: true,
    })

    action = options.action || await promptText({
      message: 'Action (allow or deny)',
      required: true,
    })

    const resolved = resolveIpAccessAction(action)
    if (resolved === undefined) {
      warning(`Invalid action: ${action}. Available: allow, deny`)
      return
    }
    action = resolved

    reason = options.description || await promptText({
      message: 'Description/reason (optional)',
    }) || null
  }

  const rule = await withSpinner('Creating IP access control rule...', async () => {
    const { data, error } = await createIpAccessControl({
      client,
      body: {
        ip_address: ipAddress,
        action,
        reason,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`IP access control rule created (ID: ${rule?.id})`)
  info(`${ipAddress} -> ${action}`)
}

async function showIpAccessAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid rule ID')
    return
  }

  const rule = await withSpinner('Fetching rule...', async () => {
    const { data, error } = await getIpAccessControl({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Rule ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(rule)
    return
  }

  newline()
  header(`${icons.info} IP Access Control Rule #${rule.id}`)
  keyValue('IP Address', rule.ip_address)
  keyValue('Action', rule.action === 'allow' ? colors.success('allow') : colors.error(rule.action))
  keyValue('Reason', rule.reason || '-')
  keyValue('Created By', rule.created_by != null ? rule.created_by.toString() : '-')
  keyValue('Created', new Date(rule.created_at).toLocaleString())
  keyValue('Updated', new Date(rule.updated_at).toLocaleString())
  newline()
}

async function updateIpAccessAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid rule ID')
    return
  }

  const body: Record<string, unknown> = {}

  if (options.ip) {
    body.ip_address = options.ip
  }

  if (options.action) {
    const resolved = resolveIpAccessAction(options.action)
    if (resolved === undefined) {
      warning(`Invalid action: ${options.action}. Available: allow, deny`)
      return
    }
    body.action = resolved
  }

  if (options.description !== undefined) {
    body.reason = options.description
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to update. Provide at least one of --ip, --action, or --description')
    return
  }

  await withSpinner('Updating rule...', async () => {
    const { data, error } = await updateIpAccessControl({
      client,
      path: { id },
      body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Rule #${id} updated`)

  if (options.ip) {
    info(`IP: ${options.ip}`)
  }
  if (options.action) {
    info(`Action: ${options.action}`)
  }
  if (options.description !== undefined) {
    info(`Description: ${options.description || '(cleared)'}`)
  }
}

async function removeIpAccessAction(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid rule ID')
    return
  }

  // Get rule details first for confirmation message
  const { data: rule, error: getError } = await getIpAccessControl({
    client,
    path: { id },
  })

  if (getError || !rule) {
    warning(`Rule ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete IP access control rule for ${rule.ip_address} (${rule.action})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting rule...', async () => {
    const { error } = await deleteIpAccessControl({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Rule #${id} deleted (${rule.ip_address})`)
}

async function checkIpAction(options: CheckOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let ipAddress: string

  if (options.ip) {
    ipAddress = options.ip
  } else {
    ipAddress = await promptText({
      message: 'IP address to check',
      required: true,
    })
  }

  const result = await withSpinner(`Checking IP ${ipAddress}...`, async () => {
    const { data, error } = await checkIpBlocked({
      client,
      path: { ip: ipAddress },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} IP Check Result: ${ipAddress}`)

  if (result && typeof result === 'object') {
    const resultObj = result as Record<string, unknown>
    for (const [key, value] of Object.entries(resultObj)) {
      keyValue(key, String(value))
    }
  } else {
    info(`Result: ${JSON.stringify(result)}`)
  }
  newline()
}
