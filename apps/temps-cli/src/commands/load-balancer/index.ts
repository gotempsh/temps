import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listRoutes,
  createRoute,
  getRoute,
  updateRoute,
  deleteRoute,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, keyValue } from '../../ui/output.js'

interface ListOptions {
  json?: boolean
}

interface CreateOptions {
  domain?: string
  target?: string
  yes?: boolean
}

interface ShowOptions {
  domain: string
  json?: boolean
}

interface UpdateOptions {
  domain: string
  target?: string
}

interface RemoveOptions {
  domain: string
  force?: boolean
  yes?: boolean
}

/** Split a user-supplied target (with or without a scheme) into host + port, defaulting the port from the scheme. */
export function parseTarget(target: string): { host: string; port: number } {
  const targetUrl = new URL(target.includes('://') ? target : `http://${target}`)
  const host = targetUrl.hostname
  const port = parseInt(targetUrl.port || (targetUrl.protocol === 'https:' ? '443' : '80'), 10)
  return { host, port }
}

export function registerLoadBalancerCommands(program: Command): void {
  const lb = program
    .command('load-balancer')
    .alias('lb')
    .description('Manage load balancer routes')

  lb
    .command('list')
    .alias('ls')
    .description('List load balancer routes')
    .option('--json', 'Output in JSON format')
    .action(listRoutesAction)

  lb
    .command('create')
    .alias('add')
    .description('Create a load balancer route')
    .option('-d, --domain <domain>', 'Domain for the route')
    .option('-t, --target <target>', 'Target upstream URL')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createRouteAction)

  lb
    .command('show')
    .description('Show route details')
    .requiredOption('-d, --domain <domain>', 'Domain of the route')
    .option('--json', 'Output in JSON format')
    .action(showRoute)

  lb
    .command('update')
    .description('Update a load balancer route')
    .requiredOption('-d, --domain <domain>', 'Domain of the route')
    .option('-t, --target <target>', 'New target upstream URL')
    .action(updateRouteAction)

  lb
    .command('remove')
    .alias('rm')
    .description('Delete a load balancer route')
    .requiredOption('-d, --domain <domain>', 'Domain of the route')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeRoute)
}

async function listRoutesAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const routes = await withSpinner('Fetching routes...', async () => {
    const { data, error } = await listRoutes({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(routes)
    return
  }

  newline()
  header(`${icons.globe} Load Balancer Routes (${routes.length})`)

  if (routes.length === 0) {
    info('No load balancer routes configured')
    info('Run: temps lb create --domain example.com --target http://localhost:8080 -y')
    newline()
    return
  }

  const columns: TableColumn<Record<string, unknown>>[] = [
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Target', key: 'target', color: (v) => colors.muted(v) },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v === 'active' ? 'active' : v === 'error' ? 'error' : 'inactive') },
    { header: 'SSL', accessor: (r) => (r as Record<string, unknown>).ssl_enabled ? 'Yes' : 'No' },
    { header: 'Created', accessor: (r) => (r as Record<string, unknown>).created_at ? new Date((r as Record<string, unknown>).created_at as string).toLocaleDateString() : '-' },
  ]

  printTable(routes as Record<string, unknown>[], columns, { style: 'minimal' })
  newline()
}

async function createRouteAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let domain: string
  let target: string

  const isAutomation = options.yes && options.domain && options.target

  if (isAutomation) {
    domain = options.domain!
    target = options.target!
  } else {
    domain = options.domain || await promptText({
      message: 'Domain',
      required: true,
    })

    target = options.target || await promptText({
      message: 'Target upstream URL (e.g., http://localhost:8080)',
      required: true,
    })

    if (!options.yes) {
      newline()
      info(`Domain: ${domain}`)
      info(`Target: ${target}`)
      newline()

      const confirmed = await promptConfirm({
        message: 'Create this route?',
        default: true,
      })
      if (!confirmed) {
        info('Cancelled')
        return
      }
    }
  }

  const { host, port } = parseTarget(target)

  await withSpinner(`Creating route for ${domain}...`, async () => {
    const { error } = await createRoute({
      client,
      body: {
        domain,
        host,
        port,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Route created for ${domain}`)
  info(`Target: ${target}`)
}

async function showRoute(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domain = options.domain

  const route = await withSpinner('Fetching route...', async () => {
    const { data, error } = await getRoute({
      client,
      path: { domain },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Route for ${domain} not found`)
    }
    return data
  })

  if (options.json) {
    json(route)
    return
  }

  const routeData = route as Record<string, unknown>

  newline()
  header(`${icons.globe} Route: ${routeData.domain}`)
  keyValue('Domain', routeData.domain as string)
  keyValue('Target', routeData.target as string)
  if (routeData.status !== undefined) {
    keyValue('Status', statusBadge(
      (routeData.status as string) === 'active' ? 'active' :
      (routeData.status as string) === 'error' ? 'error' : 'inactive'
    ))
  }
  if (routeData.ssl_enabled !== undefined) {
    keyValue('SSL', routeData.ssl_enabled ? colors.success('Enabled') : colors.muted('Disabled'))
  }
  if (routeData.health_check_path) {
    keyValue('Health Check', routeData.health_check_path as string)
  }
  if (routeData.load_balance_strategy) {
    keyValue('Strategy', routeData.load_balance_strategy as string)
  }
  if (routeData.created_at) {
    keyValue('Created', new Date(routeData.created_at as string).toLocaleString())
  }
  if (routeData.updated_at) {
    keyValue('Updated', new Date(routeData.updated_at as string).toLocaleString())
  }
  newline()
}

async function updateRouteAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domain = options.domain

  let target: string

  if (options.target) {
    target = options.target
  } else {
    target = await promptText({
      message: 'New target upstream URL',
      required: true,
    })
  }

  const { host, port } = parseTarget(target)

  await withSpinner(`Updating route for ${domain}...`, async () => {
    const { error } = await updateRoute({
      client,
      path: { domain },
      body: {
        enabled: true,
        host,
        port,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Route updated for ${domain}`)
  info(`New target: ${target}`)
}

async function removeRoute(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domain = options.domain
  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove load balancer route for "${domain}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner(`Removing route for ${domain}...`, async () => {
    const { error } = await deleteRoute({
      client,
      path: { domain },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Route for ${domain} removed`)
}
