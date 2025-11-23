import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm, wizard } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, box } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Provider {
  id: number
  name: string
  type: string
  status: string
  created_at: string
}

export function registerProvidersCommands(program: Command): void {
  const providers = program
    .command('providers')
    .alias('provider')
    .description('Manage service providers (Git, storage, databases)')

  providers
    .command('list')
    .alias('ls')
    .description('List configured providers')
    .option('-t, --type <type>', 'Filter by type (git, storage, database)')
    .option('--json', 'Output in JSON format')
    .action(listProviders)

  providers
    .command('add')
    .description('Add a new provider (interactive)')
    .action(addProvider)

  providers
    .command('remove <provider>')
    .alias('rm')
    .description('Remove a provider')
    .option('-f, --force', 'Skip confirmation')
    .action(removeProvider)

  providers
    .command('test <provider>')
    .description('Test provider connection')
    .action(testProvider)

  // Git-specific commands
  const git = providers.command('git').description('Manage Git providers')

  git
    .command('connect <provider>')
    .description('Connect a Git provider (github, gitlab, bitbucket)')
    .action(connectGitProvider)

  git
    .command('repos [provider]')
    .description('List available repositories')
    .action(listRepos)
}

async function listProviders(options: { type?: string; json?: boolean }): Promise<void> {
  await requireAuth()
  const client = getClient()

  const providers = await withSpinner('Fetching providers...', async () => {
    const response = await client.get('/api/providers' as never, {
      params: { query: { type: options.type } },
    })
    return (response.data ?? []) as Provider[]
  })

  if (options.json) {
    json(providers)
    return
  }

  newline()
  header(`${icons.package} Providers (${providers.length})`)

  const columns: TableColumn<Provider>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'type' },
    { header: 'Status', accessor: (p) => p.status, color: (v) => statusBadge(v) },
  ]

  printTable(providers, columns, { style: 'minimal' })
  newline()
}

async function addProvider(): Promise<void> {
  await requireAuth()

  const providerType = await promptSelect({
    message: 'Provider type',
    choices: [
      { name: 'Git Provider', value: 'git', description: 'GitHub, GitLab, Bitbucket' },
      { name: 'Object Storage', value: 'storage', description: 'S3, MinIO, Cloudflare R2' },
      { name: 'Database', value: 'database', description: 'PostgreSQL, MySQL, MongoDB' },
      { name: 'Registry', value: 'registry', description: 'Docker Hub, GHCR, ECR' },
    ],
  })

  switch (providerType) {
    case 'git':
      await addGitProvider()
      break
    case 'storage':
      await addStorageProvider()
      break
    case 'database':
      await addDatabaseProvider()
      break
    case 'registry':
      await addRegistryProvider()
      break
  }
}

async function addGitProvider(): Promise<void> {
  const provider = await promptSelect({
    message: 'Git provider',
    choices: [
      { name: 'GitHub', value: 'github' },
      { name: 'GitLab', value: 'gitlab' },
      { name: 'Bitbucket', value: 'bitbucket' },
    ],
  })

  info(`\nTo connect ${provider}, you'll need to create a personal access token.`)

  const tokenUrl: Record<string, string> = {
    github: 'https://github.com/settings/tokens/new',
    gitlab: 'https://gitlab.com/-/profile/personal_access_tokens',
    bitbucket: 'https://bitbucket.org/account/settings/app-passwords/',
  }

  info(`Visit: ${colors.primary(tokenUrl[provider])}\n`)

  const token = await promptPassword({
    message: 'Personal access token',
  })

  const client = getClient()

  await withSpinner(`Connecting to ${provider}...`, async () => {
    await client.post('/api/providers/git' as never, {
      body: { provider, token },
    })
  })

  success(`${provider} connected successfully`)
}

async function addStorageProvider(): Promise<void> {
  const provider = await promptSelect({
    message: 'Storage provider',
    choices: [
      { name: 'AWS S3', value: 's3' },
      { name: 'MinIO', value: 'minio' },
      { name: 'Cloudflare R2', value: 'r2' },
    ],
  })

  const name = await promptText({ message: 'Provider name', required: true })
  const endpoint = await promptText({
    message: 'Endpoint URL',
    default: provider === 's3' ? 'https://s3.amazonaws.com' : undefined,
  })
  const accessKey = await promptText({ message: 'Access key', required: true })
  const secretKey = await promptPassword({ message: 'Secret key' })
  const bucket = await promptText({ message: 'Default bucket', required: true })

  const client = getClient()

  await withSpinner('Adding storage provider...', async () => {
    await client.post('/api/providers/storage' as never, {
      body: { name, type: provider, endpoint, access_key: accessKey, secret_key: secretKey, bucket },
    })
  })

  success('Storage provider added')
}

async function addDatabaseProvider(): Promise<void> {
  const provider = await promptSelect({
    message: 'Database type',
    choices: [
      { name: 'PostgreSQL', value: 'postgres' },
      { name: 'MySQL', value: 'mysql' },
      { name: 'MongoDB', value: 'mongodb' },
      { name: 'Redis', value: 'redis' },
    ],
  })

  const name = await promptText({ message: 'Provider name', required: true })
  const host = await promptText({ message: 'Host', required: true })
  const port = await promptText({
    message: 'Port',
    default: provider === 'postgres' ? '5432' : provider === 'mysql' ? '3306' : provider === 'mongodb' ? '27017' : '6379',
  })
  const username = await promptText({ message: 'Username', required: true })
  const password = await promptPassword({ message: 'Password' })
  const database = await promptText({ message: 'Database name', required: true })

  const client = getClient()

  await withSpinner('Adding database provider...', async () => {
    await client.post('/api/providers/database' as never, {
      body: { name, type: provider, host, port: parseInt(port), username, password, database },
    })
  })

  success('Database provider added')
}

async function addRegistryProvider(): Promise<void> {
  const provider = await promptSelect({
    message: 'Registry type',
    choices: [
      { name: 'Docker Hub', value: 'dockerhub' },
      { name: 'GitHub Container Registry', value: 'ghcr' },
      { name: 'AWS ECR', value: 'ecr' },
      { name: 'Custom Registry', value: 'custom' },
    ],
  })

  const name = await promptText({ message: 'Provider name', required: true })
  const username = await promptText({ message: 'Username', required: true })
  const password = await promptPassword({ message: 'Password/Token' })

  let endpoint: string | undefined
  if (provider === 'custom') {
    endpoint = await promptText({ message: 'Registry URL', required: true })
  }

  const client = getClient()

  await withSpinner('Adding registry provider...', async () => {
    await client.post('/api/providers/registry' as never, {
      body: { name, type: provider, username, password, endpoint },
    })
  })

  success('Registry provider added')
}

async function removeProvider(providerId: string, options: { force?: boolean }): Promise<void> {
  await requireAuth()

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Remove provider ${providerId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const client = getClient()

  await withSpinner('Removing provider...', async () => {
    await client.delete('/api/providers/{id}' as never, {
      params: { path: { id: providerId } },
    })
  })

  success('Provider removed')
}

async function testProvider(providerId: string): Promise<void> {
  await requireAuth()
  const client = getClient()

  const result = await withSpinner('Testing connection...', async () => {
    const response = await client.post('/api/providers/{id}/test' as never, {
      params: { path: { id: providerId } },
    })
    return response.data as { success: boolean; message?: string; latency_ms?: number }
  })

  newline()
  if (result.success) {
    success(`Connection successful${result.latency_ms ? ` (${result.latency_ms}ms)` : ''}`)
  } else {
    warning(`Connection failed: ${result.message ?? 'Unknown error'}`)
  }
}

async function connectGitProvider(provider: string): Promise<void> {
  await addGitProvider()
}

async function listRepos(provider?: string): Promise<void> {
  await requireAuth()
  const client = getClient()

  const repos = await withSpinner('Fetching repositories...', async () => {
    const response = await client.get('/api/providers/git/repos' as never, {
      params: { query: { provider } },
    })
    return (response.data ?? []) as Array<{
      name: string
      full_name: string
      default_branch: string
      private: boolean
    }>
  })

  newline()
  header(`${icons.folder} Available Repositories (${repos.length})`)

  for (const repo of repos) {
    const visibility = repo.private ? colors.muted('(private)') : colors.success('(public)')
    console.log(`  ${colors.bold(repo.full_name)} ${visibility}`)
    console.log(`    ${colors.muted(`Branch: ${repo.default_branch}`)}`)
  }

  newline()
}
