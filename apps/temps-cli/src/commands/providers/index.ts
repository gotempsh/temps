import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listGitProviders,
  createGithubPatProvider,
  createGitlabPatProvider,
  createGiteaPatProvider,
  createBitbucketProvider,
  createGenericProvider,
  deleteGitProvider,
  getGitProvider,
  listSyncedRepositories,
  listRepositoriesByConnection,
  activateProvider,
  deactivateProvider,
  deleteProviderSafely,
  checkProviderDeletionSafety,
  getProviderConnections,
  listConnections,
  deleteConnection,
  activateConnection,
  deactivateConnection,
  syncRepositories,
  updateConnectionToken,
  validateConnection,
} from '../../api/sdk.gen.js'
import type { ProviderResponse, ConnectionResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface AddOptions {
  provider?: string
  name?: string
  token?: string
  baseUrl?: string
  // Bitbucket app-password auth
  username?: string
  password?: string
  // Generic (Other) HTTPS provider
  cloneUrl?: string
  tokenUsername?: string
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

interface ConnectOptions {
  provider: string
  name?: string
  token?: string
  baseUrl?: string
  username?: string
  password?: string
  cloneUrl?: string
  tokenUsername?: string
  yes?: boolean
}

interface ReposOptions {
  id?: string
  json?: boolean
  search?: string
  page?: string
  perPage?: string
  sort?: string
  direction?: string
  language?: string
  owner?: string
}

interface IdOptions {
  id: string
}

interface IdJsonOptions {
  id: string
  json?: boolean
}

interface IdForceOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface UpdateTokenOptions {
  id: string
  token: string
}

export function registerProvidersCommands(program: Command): void {
  const providers = program
    .command('providers')
    .alias('provider')
    .description('Manage Git providers')

  providers
    .command('list')
    .alias('ls')
    .description('List configured Git providers')
    .option('--json', 'Output in JSON format')
    .action(listProviders)

  providers
    .command('add')
    .description('Add a new Git provider')
    .option('-p, --provider <provider>', 'Provider type (github, gitlab, bitbucket, gitea, generic)')
    .option('-n, --name <name>', 'Provider name')
    .option('-t, --token <token>', 'Personal access token (or Bitbucket access token / app password)')
    .option('--base-url <url>', 'Instance base URL (GitLab/Gitea self-hosted; required for gitea)')
    .option('--username <username>', 'Bitbucket username (selects app-password auth)')
    .option('--password <password>', 'Bitbucket app password (used with --username)')
    .option('--clone-url <url>', 'HTTPS clone URL (generic provider)')
    .option('--token-username <username>', 'HTTP Basic username for the token (generic; default x-access-token)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(addProvider)

  providers
    .command('remove')
    .alias('rm')
    .description('Remove a Git provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeProvider)

  providers
    .command('show')
    .description('Show Git provider details')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showProvider)

  providers
    .command('activate')
    .description('Activate a Git provider')
    .requiredOption('--id <id>', 'Provider ID')
    .action(activateProviderAction)

  providers
    .command('deactivate')
    .description('Deactivate a Git provider')
    .requiredOption('--id <id>', 'Provider ID')
    .action(deactivateProviderAction)

  providers
    .command('safe-delete')
    .description('Safely delete a Git provider (checks dependencies first)')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(safeDeleteProviderAction)

  providers
    .command('deletion-check')
    .description('Check if a Git provider can be safely deleted')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(deletionCheckAction)

  // Git-specific commands
  const git = providers.command('git').description('Manage Git providers')

  git
    .command('connect')
    .description('Connect a Git provider (github, gitlab, bitbucket, gitea, generic)')
    .requiredOption('-p, --provider <provider>', 'Provider type (github, gitlab, bitbucket, gitea, generic)')
    .option('-n, --name <name>', 'Provider name')
    .option('-t, --token <token>', 'Personal access token (or Bitbucket access token / app password)')
    .option('--base-url <url>', 'Instance base URL (GitLab/Gitea self-hosted; required for gitea)')
    .option('--username <username>', 'Bitbucket username (selects app-password auth)')
    .option('--password <password>', 'Bitbucket app password (used with --username)')
    .option('--clone-url <url>', 'HTTPS clone URL (generic provider)')
    .option('--token-username <username>', 'HTTP Basic username for the token (generic; default x-access-token)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(connectGitProvider)

  git
    .command('repos')
    .description('List available repositories')
    .option('--id <id>', 'Provider ID (optional, lists all if not provided)')
    .option('--json', 'Output in JSON format')
    .option('--search <term>', 'Search repositories by name')
    .option('--page <n>', 'Page number')
    .option('--per-page <n>', 'Items per page (max: 100)')
    .option('--sort <field>', 'Sort by field (name, created_at, updated_at, stars)')
    .option('--direction <dir>', 'Sort direction: asc or desc')
    .option('--language <lang>', 'Filter by programming language')
    .option('--owner <owner>', 'Filter by repository owner')
    .action(listRepos)

  // Connections subcommands
  const connections = providers.command('connections').alias('conn').description('Manage Git provider connections')

  connections
    .command('list')
    .alias('ls')
    .description('List all Git connections')
    .option('--json', 'Output in JSON format')
    .option('--page <n>', 'Page number')
    .option('--per-page <n>', 'Items per page (default: 30, max: 100)')
    .option('--sort <field>', 'Sort by field (created_at, updated_at, account_name)')
    .option('--direction <dir>', 'Sort direction: asc or desc (default: desc)')
    .action(listConnectionsAction)

  connections
    .command('show')
    .description('Show connection details for a provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showConnectionsAction)

  connections
    .command('delete')
    .alias('rm')
    .description('Delete a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(deleteConnectionAction)

  connections
    .command('activate')
    .description('Activate a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .action(activateConnectionAction)

  connections
    .command('deactivate')
    .description('Deactivate a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .action(deactivateConnectionAction)

  connections
    .command('sync')
    .description('Sync repositories for a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .action(syncConnectionAction)

  connections
    .command('update-token')
    .description('Update access token for a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .requiredOption('-t, --token <token>', 'New access token')
    .action(updateTokenAction)

  connections
    .command('validate')
    .description('Validate a Git connection')
    .requiredOption('--id <id>', 'Connection ID')
    .option('--json', 'Output in JSON format')
    .action(validateConnectionAction)
}

async function listProviders(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const providers = await withSpinner('Fetching providers...', async () => {
    const { data, error } = await listGitProviders({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(providers)
    return
  }

  newline()
  header(`${icons.package} Git Providers (${providers.length})`)

  if (providers.length === 0) {
    info('No Git providers configured')
    info('Run: temps providers add --provider github --name my-github --token <token> -y')
    newline()
    return
  }

  const columns: TableColumn<ProviderResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'provider_type' },
    { header: 'Auth', key: 'auth_method' },
    { header: 'Status', accessor: (p) => p.is_active ? 'active' : 'inactive', color: (v) => statusBadge(v) },
  ]

  printTable(providers, columns, { style: 'minimal' })
  newline()
}

const SUPPORTED_PROVIDERS = ['github', 'gitlab', 'bitbucket', 'gitea', 'generic']

async function addProvider(options: AddOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const interactive = !options.yes

  // 1. Provider
  let provider = options.provider
  if (!provider && interactive) {
    provider = await promptSelect({
      message: 'Git provider',
      choices: [
        { name: 'GitHub', value: 'github' },
        { name: 'GitLab', value: 'gitlab' },
        { name: 'Bitbucket', value: 'bitbucket' },
        { name: 'Gitea / Forgejo', value: 'gitea' },
        { name: 'Other Git Providers (generic HTTPS)', value: 'generic' },
      ],
    })
  }
  if (!provider || !SUPPORTED_PROVIDERS.includes(provider)) {
    warning(`Invalid provider: ${provider}. Supported: ${SUPPORTED_PROVIDERS.join(', ')}`)
    return
  }

  // 2. Name (shared by all providers)
  const name = options.name || (interactive
    ? await promptText({ message: 'Provider name', default: `${provider}-connection`, required: true })
    : '')
  if (!name) {
    warning('Provider name is required (use -n/--name)')
    return
  }

  // Helper: prompt for a required PAT unless supplied
  const getToken = async (): Promise<string> => {
    const t = options.token || (interactive ? await promptPassword({ message: 'Personal access token' }) : '')
    return t
  }

  await withSpinner(`Connecting to ${provider}...`, async () => {
    if (provider === 'github') {
      const token = await getToken()
      if (!token) throw new Error('A token is required (use -t/--token)')
      const { error } = await createGithubPatProvider({ client, body: { name, token } })
      if (error) throw new Error(getErrorMessage(error))
    } else if (provider === 'gitlab') {
      const token = await getToken()
      if (!token) throw new Error('A token is required (use -t/--token)')
      const baseUrl = options.baseUrl || (interactive
        ? (await promptText({ message: 'GitLab base URL (leave empty for gitlab.com)', default: '' }) || null)
        : null)
      const { error } = await createGitlabPatProvider({ client, body: { name, token, base_url: baseUrl } })
      if (error) throw new Error(getErrorMessage(error))
    } else if (provider === 'gitea') {
      const baseUrl = options.baseUrl || (interactive
        ? await promptText({ message: 'Gitea instance URL (https://...)', required: true })
        : '')
      if (!baseUrl) throw new Error('Gitea requires --base-url (the HTTPS instance URL)')
      const token = await getToken()
      if (!token) throw new Error('A token is required (use -t/--token)')
      const { error } = await createGiteaPatProvider({ client, body: { name, token, base_url: baseUrl } })
      if (error) throw new Error(getErrorMessage(error))
    } else if (provider === 'bitbucket') {
      // Access token, or app password (username + password). --username selects app-password auth.
      let auth: { type: 'access_token'; token: string } | { type: 'app_password'; username: string; password: string }
      if (options.username) {
        const password = options.password || options.token || (interactive ? await promptPassword({ message: 'Bitbucket app password' }) : '')
        if (!password) throw new Error('Bitbucket app password is required (use --password or -t/--token)')
        auth = { type: 'app_password', username: options.username, password }
      } else if (options.token || !interactive) {
        if (!options.token) throw new Error('Bitbucket access token is required (use -t/--token), or --username for app-password auth')
        auth = { type: 'access_token', token: options.token }
      } else {
        const method = await promptSelect({
          message: 'Bitbucket authentication',
          choices: [
            { name: 'Access token (recommended)', value: 'access_token' },
            { name: 'App password', value: 'app_password' },
          ],
        })
        if (method === 'app_password') {
          const username = await promptText({ message: 'Atlassian username', required: true })
          const password = await promptPassword({ message: 'App password' })
          auth = { type: 'app_password', username, password }
        } else {
          const token = await promptPassword({ message: 'Access token' })
          auth = { type: 'access_token', token }
        }
      }
      const { error } = await createBitbucketProvider({ client, body: { name, auth } })
      if (error) throw new Error(getErrorMessage(error))
    } else if (provider === 'generic') {
      const cloneUrl = options.cloneUrl || (interactive
        ? await promptText({ message: 'HTTPS clone URL (https://...)', required: true })
        : '')
      if (!cloneUrl) throw new Error('The generic provider requires --clone-url (an HTTPS clone URL)')
      let token: string | null = options.token ?? null
      if (token === null && interactive) {
        token = (await promptText({ message: 'Access token (leave empty for a public repo)', default: '' })) || null
      }
      const { error } = await createGenericProvider({
        client,
        body: {
          name,
          clone_url: cloneUrl,
          base_url: options.baseUrl ?? null,
          token,
          token_username: options.tokenUsername ?? null,
        },
      })
      if (error) throw new Error(getErrorMessage(error))
    }
  })

  success(`${provider} connected successfully`)
}

async function removeProvider(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Get provider details first
  const { data: provider, error: getError } = await getGitProvider({
    client,
    path: { provider_id: id },
  })

  if (getError || !provider) {
    warning(`Provider ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove provider "${provider.name}" (${provider.provider_type})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing provider...', async () => {
    const { error } = await deleteGitProvider({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Provider removed')
}

async function showProvider(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const provider = await withSpinner('Fetching provider...', async () => {
    const { data, error } = await getGitProvider({
      client,
      path: { provider_id: id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Provider ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(provider)
    return
  }

  newline()
  header(`${icons.package} ${provider.name}`)
  console.log(`  ${colors.muted('ID:')} ${provider.id}`)
  console.log(`  ${colors.muted('Type:')} ${provider.provider_type}`)
  console.log(`  ${colors.muted('Auth Method:')} ${provider.auth_method}`)
  console.log(`  ${colors.muted('Status:')} ${statusBadge(provider.is_active ? 'active' : 'inactive')}`)
  if (provider.base_url) {
    console.log(`  ${colors.muted('Base URL:')} ${provider.base_url}`)
  }
  console.log(`  ${colors.muted('Created:')} ${provider.created_at}`)
  newline()
}

async function activateProviderAction(options: IdOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  await withSpinner('Activating provider...', async () => {
    const { error } = await activateProvider({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Provider ${id} activated`)
}

async function deactivateProviderAction(options: IdOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  await withSpinner('Deactivating provider...', async () => {
    const { error } = await deactivateProvider({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Provider ${id} deactivated`)
}

async function safeDeleteProviderAction(options: IdForceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Check deletion safety first
  const check = await withSpinner('Checking deletion safety...', async () => {
    const { data, error } = await checkProviderDeletionSafety({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!check?.can_delete) {
    warning(check?.message ?? 'Provider cannot be safely deleted')
    if (check?.projects_in_use && check.projects_in_use.length > 0) {
      newline()
      info('Projects using this provider:')
      for (const project of check.projects_in_use) {
        console.log(`  ${colors.muted('-')} ${colors.bold(project.name)} (ID: ${project.id}, slug: ${project.slug})`)
      }
      newline()
    }
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Safely delete provider ${id}? This action cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting provider...', async () => {
    const { error } = await deleteProviderSafely({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Provider ${id} safely deleted`)
}

async function deletionCheckAction(options: IdJsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const check = await withSpinner('Checking deletion safety...', async () => {
    const { data, error } = await checkProviderDeletionSafety({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(check)
    return
  }

  newline()
  header(`${icons.info} Deletion Check for Provider ${id}`)
  keyValue('Can Delete', check?.can_delete ? colors.success('yes') : colors.error('no'))
  keyValue('Message', check?.message ?? 'N/A')

  if (check?.projects_in_use && check.projects_in_use.length > 0) {
    newline()
    info('Projects using this provider:')
    for (const project of check.projects_in_use) {
      console.log(`  ${colors.muted('-')} ${colors.bold(project.name)} (ID: ${project.id}, slug: ${project.slug})`)
    }
  }

  newline()
}

async function connectGitProvider(options: ConnectOptions): Promise<void> {
  if (!SUPPORTED_PROVIDERS.includes(options.provider)) {
    warning(`Unsupported provider: ${options.provider}. Supported: ${SUPPORTED_PROVIDERS.join(', ')}`)
    return
  }

  await addProvider({
    provider: options.provider,
    name: options.name,
    token: options.token,
    baseUrl: options.baseUrl,
    username: options.username,
    password: options.password,
    cloneUrl: options.cloneUrl,
    tokenUsername: options.tokenUsername,
    yes: options.yes,
  })
}

async function listRepos(options: ReposOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const page = options.page ? parseInt(options.page, 10) : undefined
  const perPage = options.perPage ? parseInt(options.perPage, 10) : undefined

  const repoQuery = {
    ...(page && { page }),
    ...(perPage && { per_page: perPage }),
    ...(options.sort && { sort: options.sort }),
    ...(options.direction && { direction: options.direction }),
    ...(options.search && { search: options.search }),
    ...(options.language && { language: options.language }),
    ...(options.owner && { owner: options.owner }),
  }

  const repos = await withSpinner('Fetching repositories...', async () => {
    if (options.id) {
      const id = parseInt(options.id, 10)
      if (isNaN(id)) {
        throw new Error('Invalid provider ID')
      }
      const { data, error } = await listRepositoriesByConnection({
        client,
        path: { connection_id: id },
        query: repoQuery,
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data?.repositories ?? []
    } else {
      const { data, error } = await listSyncedRepositories({
        client,
        query: repoQuery,
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data?.repositories ?? []
    }
  })

  if (options.json) {
    json(repos)
    return
  }

  newline()
  header(`${icons.folder} Available Repositories (${repos.length})`)

  if (repos.length === 0) {
    info('No repositories found')
    info('Sync repositories from your Git provider in the web dashboard')
    newline()
    return
  }

  for (const repo of repos) {
    const visibility = repo.private ? colors.muted('(private)') : colors.success('(public)')
    console.log(`  ${colors.bold(repo.full_name)} ${visibility}`)
    console.log(`    ${colors.muted(`Branch: ${repo.default_branch}`)}`)
  }

  newline()
}

// --- Connection subcommands ---

interface ConnectionsListOptions {
  json?: boolean
  page?: string
  perPage?: string
  sort?: string
  direction?: string
}

async function listConnectionsAction(options: ConnectionsListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const page = options.page ? parseInt(options.page, 10) : undefined
  const perPage = options.perPage ? parseInt(options.perPage, 10) : undefined

  const result = await withSpinner('Fetching connections...', async () => {
    const { data, error } = await listConnections({
      client,
      query: {
        ...(page && { page }),
        ...(perPage && { per_page: perPage }),
        ...(options.sort && { sort: options.sort }),
        ...(options.direction && { direction: options.direction }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const connectionsList = result?.connections ?? []

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.globe} Git Connections (${connectionsList.length})`)

  if (connectionsList.length === 0) {
    info('No Git connections found')
    info('Add a provider first: temps providers add')
    newline()
    return
  }

  const columns: TableColumn<ConnectionResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Account', key: 'account_name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'account_type' },
    { header: 'Provider', key: 'provider_id' },
    { header: 'Status', accessor: (c) => c.is_active ? 'active' : 'inactive', color: (v) => statusBadge(v) },
    { header: 'Expired', accessor: (c) => c.is_expired ? 'yes' : 'no', color: (v) => v === 'yes' ? colors.error(v) : colors.success(v) },
    { header: 'Syncing', accessor: (c) => c.syncing ? 'yes' : 'no' },
  ]

  printTable(connectionsList, columns, { style: 'minimal' })
  newline()
}

async function showConnectionsAction(options: IdJsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const connectionsList = await withSpinner('Fetching connections...', async () => {
    const { data, error } = await getProviderConnections({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(connectionsList)
    return
  }

  newline()
  header(`${icons.globe} Connections for Provider ${id} (${connectionsList.length})`)

  if (connectionsList.length === 0) {
    info('No connections found for this provider')
    newline()
    return
  }

  for (const conn of connectionsList) {
    newline()
    console.log(`  ${colors.bold(conn.account_name)} ${colors.muted(`(ID: ${conn.id})`)}`)
    keyValue('Account Type', conn.account_type)
    keyValue('Status', statusBadge(conn.is_active ? 'active' : 'inactive'))
    keyValue('Expired', conn.is_expired ? colors.error('yes') : colors.success('no'))
    keyValue('Syncing', conn.syncing ? 'yes' : 'no')
    keyValue('Last Synced', conn.last_synced_at ?? 'never')
    keyValue('Created', conn.created_at)
  }

  newline()
}

async function deleteConnectionAction(options: IdForceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete connection ${id}? This action cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting connection...', async () => {
    const { error } = await deleteConnection({
      client,
      path: { connection_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Connection ${id} deleted`)
}

async function activateConnectionAction(options: IdOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  await withSpinner('Activating connection...', async () => {
    const { error } = await activateConnection({
      client,
      path: { connection_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Connection ${id} activated`)
}

async function deactivateConnectionAction(options: IdOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  await withSpinner('Deactivating connection...', async () => {
    const { error } = await deactivateConnection({
      client,
      path: { connection_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Connection ${id} deactivated`)
}

async function syncConnectionAction(options: IdOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  const result = await withSpinner('Syncing repositories...', async () => {
    const { data, error } = await syncRepositories({
      client,
      path: { connection_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Synced ${result?.total_count ?? 0} repositories for connection ${id}`)
  if (result?.synced_at) {
    info(`Synced at: ${result.synced_at}`)
  }
}

async function updateTokenAction(options: UpdateTokenOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  const result = await withSpinner('Updating token...', async () => {
    const { data, error } = await updateConnectionToken({
      client,
      path: { connection_id: id },
      body: { access_token: options.token },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(result?.message ?? `Token updated for connection ${id}`)
}

async function validateConnectionAction(options: IdJsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid connection ID')
    return
  }

  const result = await withSpinner('Validating connection...', async () => {
    const { data, error } = await validateConnection({
      client,
      path: { connection_id: id },
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
  header(`${icons.info} Connection Validation (ID: ${id})`)
  keyValue('Valid', result?.is_valid ? colors.success('yes') : colors.error('no'))
  keyValue('Message', result?.message ?? 'N/A')
  newline()

  if (result?.is_valid) {
    success('Connection is valid')
  } else {
    warning('Connection validation failed')
  }
}
