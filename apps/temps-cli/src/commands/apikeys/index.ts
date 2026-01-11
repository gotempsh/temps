import type { Command } from 'commander'
import {
  activateApiKey,
  createApiKey,
  deactivateApiKey,
  deleteApiKey,
  getApiKey,
  getApiKeyPermissions,
  listApiKeys
} from '../../api/sdk.gen.js'
import type { ApiKeyResponse } from '../../api/types.gen.js'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { colors, header, icons, info, json, keyValue, newline, success, warning } from '../../ui/output.js'
import { promptConfirm, promptSelect, promptText } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'

const ROLE_TYPES = ['admin', 'developer', 'viewer', 'readonly']

interface CreateOptions {
  name?: string
  role?: string
  expiresIn?: string
  permissions?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface ActivateDeactivateOptions {
  id: string
}

export function registerApiKeysCommands(program: Command): void {
  const apikeys = program
    .command('apikeys')
    .alias('keys')
    .description('Manage API keys for programmatic access')

  apikeys
    .command('list')
    .alias('ls')
    .description('List all API keys')
    .option('--json', 'Output in JSON format')
    .action(listApiKeysAction)

  apikeys
    .command('create')
    .alias('add')
    .description('Create a new API key')
    .option('-n, --name <name>', 'API key name')
    .option('-r, --role <role>', 'Role type (admin, developer, viewer, readonly)')
    .option('-e, --expires-in <days>', 'Expires in N days (7, 30, 90, 365)')
    .option('-p, --permissions <permissions>', 'Comma-separated list of permissions')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createApiKeyAction)

  apikeys
    .command('show')
    .description('Show API key details')
    .requiredOption('--id <id>', 'API key ID')
    .option('--json', 'Output in JSON format')
    .action(showApiKey)

  apikeys
    .command('remove')
    .alias('rm')
    .description('Delete an API key')
    .requiredOption('--id <id>', 'API key ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeApiKey)

  apikeys
    .command('activate')
    .description('Activate a deactivated API key')
    .requiredOption('--id <id>', 'API key ID')
    .action(activateApiKeyAction)

  apikeys
    .command('deactivate')
    .description('Deactivate an API key')
    .requiredOption('--id <id>', 'API key ID')
    .action(deactivateApiKeyAction)

  apikeys
    .command('permissions')
    .description('List available API key permissions')
    .option('--json', 'Output in JSON format')
    .action(listPermissions)
}

async function listApiKeysAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const keysData = await withSpinner('Fetching API keys...', async () => {
    const { data, error } = await listApiKeys({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const keys = keysData?.api_keys ?? []

  if (options.json) {
    json(keys)
    return
  }

  newline()
  header(`${icons.info} API Keys (${keys.length})`)

  if (keys.length === 0) {
    info('No API keys found')
    info('Run: temps apikeys create --name my-key --role developer -y')
    newline()
    return
  }

  const columns: TableColumn<ApiKeyResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Prefix', key: 'key_prefix' },
    { header: 'Role', key: 'role_type' },
    { header: 'Status', accessor: (k) => k.is_active ? 'active' : 'inactive', color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive') },
    { header: 'Last Used', accessor: (k) => k.last_used_at ? new Date(k.last_used_at).toLocaleDateString() : 'Never' },
    { header: 'Expires', accessor: (k) => k.expires_at ? new Date(k.expires_at).toLocaleDateString() : 'Never' },
  ]

  printTable(keys, columns, { style: 'minimal' })
  newline()
}

async function createApiKeyAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let name: string
  let roleType: string
  let expiresAt: string | null = null
  let selectedPermissions: string[] | null = null

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.name && options.role

  if (isAutomation) {
    name = options.name!
    roleType = options.role!

    // Validate role type
    if (!ROLE_TYPES.includes(roleType)) {
      warning(`Invalid role type: ${roleType}. Available: ${ROLE_TYPES.join(', ')}`)
      return
    }

    // Handle expiration
    if (options.expiresIn) {
      const days = parseInt(options.expiresIn, 10)
      if (isNaN(days) || days <= 0) {
        warning('Invalid expiration days')
        return
      }
      const expiry = new Date()
      expiry.setDate(expiry.getDate() + days)
      expiresAt = expiry.toISOString()
    }

    // Handle permissions
    if (options.permissions) {
      selectedPermissions = options.permissions.split(',').map(p => p.trim())
    }
  } else {
    // Interactive mode
    name = options.name || await promptText({
      message: 'API key name',
      required: true,
    })

    roleType = options.role || await promptSelect({
      message: 'Role type',
      choices: ROLE_TYPES.map(r => ({
        name: r.charAt(0).toUpperCase() + r.slice(1),
        value: r,
      })),
    })

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

    // Get available permissions
    const { data: permissionsData } = await getApiKeyPermissions({ client })
    const permissionsList = permissionsData?.permissions ?? []

    if (permissionsList.length > 0) {
      const useCustomPermissions = await promptConfirm({
        message: 'Customize permissions? (otherwise all permissions for role will be granted)',
        default: false,
      })

      if (useCustomPermissions) {
        info('\nAvailable permissions:')
        permissionsList.forEach((p, i) => console.log(`  ${i + 1}. ${p.name} - ${p.description}`))
        newline()

        const permInput = await promptText({
          message: 'Enter permission numbers (comma-separated)',
          default: '',
        })

        if (permInput) {
          const indices = permInput.split(',').map(s => parseInt(s.trim(), 10) - 1)
          selectedPermissions = indices
            .filter(i => i >= 0 && i < permissionsList.length)
            .map(i => permissionsList[i]!.name)
        }
      }
    }
  }

  const result = await withSpinner('Creating API key...', async () => {
    const { data, error } = await createApiKey({
      client,
      body: {
        name,
        role_type: roleType,
        expires_at: expiresAt,
        permissions: selectedPermissions,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result) {
    newline()
    success('API key created successfully')
    newline()
    header('Your API Key')
    console.log(colors.warning('⚠️  Store this key securely - it will not be shown again!'))
    newline()
    console.log(`  ${colors.bold(result.api_key)}`)
    newline()
    keyValue('ID', result.id)
    keyValue('Name', result.name)
    keyValue('Role', result.role_type)
    keyValue('Prefix', result.key_prefix)
    if (result.expires_at) {
      keyValue('Expires', new Date(result.expires_at).toLocaleString())
    }
    newline()
  }
}

async function showApiKey(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid API key ID')
    return
  }

  const key = await withSpinner('Fetching API key...', async () => {
    const { data, error } = await getApiKey({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `API key ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(key)
    return
  }

  newline()
  header(`${icons.info} ${key.name}`)
  keyValue('ID', key.id)
  keyValue('Prefix', key.key_prefix)
  keyValue('Role', key.role_type)
  keyValue('Status', key.is_active ? colors.success('Active') : colors.muted('Inactive'))
  if (key.permissions && key.permissions.length > 0) {
    keyValue('Permissions', key.permissions.join(', '))
  }
  keyValue('Created', new Date(key.created_at).toLocaleString())
  keyValue('Last Used', key.last_used_at ? new Date(key.last_used_at).toLocaleString() : colors.muted('Never'))
  keyValue('Expires', key.expires_at ? new Date(key.expires_at).toLocaleString() : colors.muted('Never'))
  newline()
}

async function removeApiKey(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid API key ID')
    return
  }

  // Get key details first
  const { data: key, error: getError } = await getApiKey({
    client,
    path: { id },
  })

  if (getError || !key) {
    warning(`API key ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('This action cannot be undone!')
    const confirmed = await promptConfirm({
      message: `Delete API key "${key.name}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting API key...', async () => {
    const { error } = await deleteApiKey({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('API key deleted')
}

async function activateApiKeyAction(options: ActivateDeactivateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid API key ID')
    return
  }

  await withSpinner('Activating API key...', async () => {
    const { error } = await activateApiKey({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('API key activated')
}

async function deactivateApiKeyAction(options: ActivateDeactivateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid API key ID')
    return
  }

  await withSpinner('Deactivating API key...', async () => {
    const { error } = await deactivateApiKey({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('API key deactivated')
  info('The key will stop working immediately')
}

async function listPermissions(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const permissionsData = await withSpinner('Fetching permissions...', async () => {
    const { data, error } = await getApiKeyPermissions({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const permissions = permissionsData?.permissions ?? []

  if (options.json) {
    json(permissions)
    return
  }

  newline()
  header(`${icons.info} Available Permissions (${permissions.length})`)

  if (permissions.length === 0) {
    info('No permissions available')
    newline()
    return
  }

  // Group permissions by category
  const categories = new Map<string, typeof permissions>()
  for (const perm of permissions) {
    const cat = perm.category || 'Other'
    if (!categories.has(cat)) {
      categories.set(cat, [])
    }
    categories.get(cat)!.push(perm)
  }

  for (const [category, perms] of categories) {
    console.log(`\n  ${colors.bold(category)}:`)
    for (const perm of perms) {
      console.log(`    ${colors.muted(perm.name)} - ${perm.description}`)
    }
  }
  newline()
}
