import type { Command } from 'commander'
import { requireAuth, config, credentials } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import { colors, header, icons, info, json, keyValue, newline, success, warning, error as errorOutput } from '../../ui/output.js'
import { promptConfirm, promptSelect, promptText } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'

// Types for deployment tokens (not in generated SDK yet)
interface DeploymentToken {
  id: number
  project_id: number
  environment_id: number | null
  name: string
  token_prefix: string
  permissions: string[] | null
  is_active: boolean
  expires_at: string | null
  last_used_at: string | null
  created_at: string
  created_by: number | null
}

interface CreateDeploymentTokenResponse {
  id: number
  project_id: number
  environment_id: number | null
  name: string
  token_prefix: string
  permissions: string[] | null
  token: string
  expires_at: string | null
  created_at: string
}

interface DeploymentTokenListResponse {
  tokens: DeploymentToken[]
  total: number
}

const PERMISSIONS = [
  { value: '*', name: 'Full Access (*)', description: 'All permissions' },
  { value: 'visitors:enrich', name: 'Visitors Enrich', description: 'Enrich visitor data' },
  { value: 'emails:send', name: 'Emails Send', description: 'Send emails' },
  { value: 'analytics:read', name: 'Analytics Read', description: 'Read analytics data' },
  { value: 'events:write', name: 'Events Write', description: 'Write custom events' },
  { value: 'errors:read', name: 'Errors Read', description: 'Read error tracking data' },
]

interface CreateOptions {
  project: string
  name?: string
  permissions?: string
  expiresIn?: string
  yes?: boolean
}

interface ListOptions {
  project: string
  json?: boolean
}

interface ShowOptions {
  project: string
  id: string
  json?: boolean
}

interface RemoveOptions {
  project: string
  id: string
  force?: boolean
  yes?: boolean
}

async function makeRequest<T>(
  method: string,
  path: string,
  body?: unknown
): Promise<T> {
  const apiUrl = config.get('apiUrl')
  const apiKey = await credentials.getApiKey()

  const response = await fetch(`${apiUrl}${path}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${apiKey}`,
    },
    body: body ? JSON.stringify(body) : undefined,
  })

  if (!response.ok) {
    const errorBody = await response.json().catch(() => ({})) as { detail?: string; title?: string }
    throw new Error(errorBody.detail || errorBody.title || `Request failed with status ${response.status}`)
  }

  if (response.status === 204) {
    return undefined as unknown as T
  }

  return response.json() as Promise<T>
}

async function resolveProjectId(projectIdentifier: string): Promise<number> {
  // Try to parse as number first
  const numId = parseInt(projectIdentifier, 10)
  if (!isNaN(numId)) {
    return numId
  }

  // Otherwise, look up by slug
  const apiUrl = config.get('apiUrl')
  const apiKey = await credentials.getApiKey()

  const response = await fetch(`${apiUrl}/api/projects?page_size=100`, {
    headers: {
      'Authorization': `Bearer ${apiKey}`,
    },
  })

  if (!response.ok) {
    throw new Error('Failed to fetch projects')
  }

  const data = await response.json() as { projects?: Array<{ slug: string; id: number }> }
  const project = data.projects?.find((p) =>
    p.slug === projectIdentifier || p.slug.toLowerCase() === projectIdentifier.toLowerCase()
  )

  if (!project) {
    throw new Error(`Project "${projectIdentifier}" not found`)
  }

  return project.id
}

export function registerTokensCommands(program: Command): void {
  const tokens = program
    .command('tokens')
    .alias('token')
    .description('Manage deployment tokens for project API access (KV, Blob, etc.)')

  tokens
    .command('list')
    .alias('ls')
    .description('List deployment tokens for a project')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(listTokensAction)

  tokens
    .command('create')
    .alias('add')
    .description('Create a new deployment token')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('-n, --name <name>', 'Token name')
    .option('--permissions <permissions>', 'Comma-separated permissions (e.g., "visitors:enrich,emails:send" or "*" for full access)')
    .option('-e, --expires-in <days>', 'Expires in N days (7, 30, 90, 365, or "never")')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createTokenAction)

  tokens
    .command('show')
    .alias('get')
    .description('Show deployment token details')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--id <id>', 'Token ID')
    .option('--json', 'Output in JSON format')
    .action(showTokenAction)

  tokens
    .command('delete')
    .alias('rm')
    .description('Delete a deployment token')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--id <id>', 'Token ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(deleteTokenAction)

  tokens
    .command('permissions')
    .description('List available deployment token permissions')
    .option('--json', 'Output in JSON format')
    .action(listPermissionsAction)
}

async function listTokensAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await withSpinner('Resolving project...', async () => {
    return resolveProjectId(options.project)
  })

  const response = await withSpinner('Fetching deployment tokens...', async () => {
    return makeRequest<DeploymentTokenListResponse>(
      'GET',
      `/api/projects/${projectId}/deployment-tokens`
    )
  })

  const tokensList = response?.tokens ?? []

  if (options.json) {
    json(tokensList)
    return
  }

  newline()
  header(`${icons.info} Deployment Tokens (${tokensList.length})`)

  if (tokensList.length === 0) {
    info('No deployment tokens found')
    info(`Run: temps tokens create -p ${options.project} --name my-token -y`)
    newline()
    return
  }

  const columns: TableColumn<DeploymentToken>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Prefix', key: 'token_prefix' },
    { header: 'Status', accessor: (t) => t.is_active ? 'active' : 'inactive', color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive') },
    { header: 'Permissions', accessor: (t) => t.permissions?.join(', ') || '*' },
    { header: 'Last Used', accessor: (t) => t.last_used_at ? new Date(t.last_used_at).toLocaleDateString() : 'Never' },
    { header: 'Expires', accessor: (t) => t.expires_at ? new Date(t.expires_at).toLocaleDateString() : 'Never' },
  ]

  printTable(tokensList, columns, { style: 'minimal' })
  newline()
}

async function createTokenAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await withSpinner('Resolving project...', async () => {
    return resolveProjectId(options.project)
  })

  let name: string
  let permissions: string[] | null = null
  let expiresAt: string | null = null

  const isAutomation = options.yes && options.name

  if (isAutomation) {
    name = options.name!

    if (options.permissions) {
      permissions = options.permissions.split(',').map(p => p.trim())
    }

    if (options.expiresIn && options.expiresIn !== 'never') {
      const days = parseInt(options.expiresIn, 10)
      if (isNaN(days) || days <= 0) {
        warning('Invalid expiration days')
        return
      }
      const expiry = new Date()
      expiry.setDate(expiry.getDate() + days)
      expiresAt = expiry.toISOString()
    }
  } else {
    // Interactive mode
    name = options.name || await promptText({
      message: 'Token name',
      required: true,
    })

    const useFullAccess = await promptConfirm({
      message: 'Grant full access (*)?',
      default: true,
    })

    if (!useFullAccess) {
      info('\nAvailable permissions:')
      PERMISSIONS.forEach((p, i) => console.log(`  ${i + 1}. ${p.name} - ${p.description}`))
      newline()

      const permInput = await promptText({
        message: 'Enter permission numbers (comma-separated)',
        default: '',
      })

      if (permInput) {
        const indices = permInput.split(',').map(s => parseInt(s.trim(), 10) - 1)
        permissions = indices
          .filter(i => i >= 0 && i < PERMISSIONS.length)
          .map(i => PERMISSIONS[i]!.value)
      }
    }

    const hasExpiry = await promptConfirm({
      message: 'Set an expiration date?',
      default: false,
    })

    if (hasExpiry) {
      const expiryDays = await promptSelect({
        message: 'Expires in',
        choices: [
          { name: '7 days', value: '7' },
          { name: '30 days', value: '30' },
          { name: '90 days', value: '90' },
          { name: '1 year', value: '365' },
        ],
      })
      const expiry = new Date()
      expiry.setDate(expiry.getDate() + parseInt(expiryDays, 10))
      expiresAt = expiry.toISOString()
    }
  }

  const result = await withSpinner('Creating deployment token...', async () => {
    return makeRequest<CreateDeploymentTokenResponse>(
      'POST',
      `/api/projects/${projectId}/deployment-tokens`,
      {
        name,
        permissions,
        expires_at: expiresAt,
      }
    )
  })

  if (result) {
    newline()
    success('Deployment token created successfully')
    newline()
    header('Your Deployment Token')
    console.log(colors.warning('⚠️  Store this token securely - it will not be shown again!'))
    newline()
    console.log(`  ${colors.bold(result.token)}`)
    newline()
    keyValue('ID', result.id)
    keyValue('Name', result.name)
    keyValue('Prefix', result.token_prefix)
    keyValue('Permissions', result.permissions?.join(', ') || '*')
    if (result.expires_at) {
      keyValue('Expires', new Date(result.expires_at).toLocaleString())
    }
    newline()
    info('Usage:')
    console.log(`  ${colors.muted('export')} TEMPS_API_URL=${config.get('apiUrl')}`)
    console.log(`  ${colors.muted('export')} TEMPS_TOKEN=${result.token}`)
    newline()
  }
}

async function showTokenAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await withSpinner('Resolving project...', async () => {
    return resolveProjectId(options.project)
  })

  const tokenId = parseInt(options.id, 10)
  if (isNaN(tokenId)) {
    warning('Invalid token ID')
    return
  }

  const token = await withSpinner('Fetching token...', async () => {
    return makeRequest<DeploymentToken>(
      'GET',
      `/api/projects/${projectId}/deployment-tokens/${tokenId}`
    )
  })

  if (options.json) {
    json(token)
    return
  }

  newline()
  header(`${icons.info} ${token.name}`)
  keyValue('ID', token.id)
  keyValue('Prefix', token.token_prefix)
  keyValue('Status', token.is_active ? colors.success('Active') : colors.muted('Inactive'))
  keyValue('Permissions', token.permissions?.join(', ') || colors.muted('Full Access (*)'))
  keyValue('Created', new Date(token.created_at).toLocaleString())
  keyValue('Last Used', token.last_used_at ? new Date(token.last_used_at).toLocaleString() : colors.muted('Never'))
  keyValue('Expires', token.expires_at ? new Date(token.expires_at).toLocaleString() : colors.muted('Never'))
  newline()
}

async function deleteTokenAction(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await withSpinner('Resolving project...', async () => {
    return resolveProjectId(options.project)
  })

  const tokenId = parseInt(options.id, 10)
  if (isNaN(tokenId)) {
    warning('Invalid token ID')
    return
  }

  // Get token details first
  const token = await withSpinner('Fetching token...', async () => {
    return makeRequest<DeploymentToken>(
      'GET',
      `/api/projects/${projectId}/deployment-tokens/${tokenId}`
    )
  })

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('This action cannot be undone!')
    const confirmed = await promptConfirm({
      message: `Delete deployment token "${token.name}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting token...', async () => {
    return makeRequest<void>(
      'DELETE',
      `/api/projects/${projectId}/deployment-tokens/${tokenId}`
    )
  })

  success('Deployment token deleted')
}

async function listPermissionsAction(options: { json?: boolean }): Promise<void> {
  if (options.json) {
    json(PERMISSIONS)
    return
  }

  newline()
  header(`${icons.info} Available Deployment Token Permissions`)
  newline()

  for (const perm of PERMISSIONS) {
    console.log(`  ${colors.bold(perm.value.padEnd(20))} ${colors.muted(perm.description)}`)
  }
  newline()
  info('Use "*" for full access to all APIs')
  newline()
}
