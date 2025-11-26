import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listGitProviders,
  createGithubPatProvider,
  createGitlabPatProvider,
  deleteProvider,
  getGitProvider,
  listSyncedRepositories,
  listRepositoriesByConnection,
} from '../../api/sdk.gen.js'
import type { ProviderResponse, RepositoryResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning } from '../../ui/output.js'

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
    .description('Add a new Git provider (interactive)')
    .action(addProvider)

  providers
    .command('remove <provider>')
    .alias('rm')
    .description('Remove a Git provider')
    .option('-f, --force', 'Skip confirmation')
    .action(removeProvider)

  providers
    .command('show <provider>')
    .description('Show Git provider details')
    .option('--json', 'Output in JSON format')
    .action(showProvider)

  // Git-specific commands
  const git = providers.command('git').description('Manage Git providers')

  git
    .command('connect <provider>')
    .description('Connect a Git provider (github, gitlab)')
    .action(connectGitProvider)

  git
    .command('repos [provider]')
    .description('List available repositories')
    .option('--json', 'Output in JSON format')
    .action(listRepos)
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
    info('Run: temps providers add')
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

async function addProvider(): Promise<void> {
  await requireAuth()
  await setupClient()

  const provider = await promptSelect({
    message: 'Git provider',
    choices: [
      { name: 'GitHub', value: 'github' },
      { name: 'GitLab', value: 'gitlab' },
    ],
  })

  info(`\nTo connect ${provider}, you'll need to create a personal access token.`)

  const tokenUrl: Record<string, string> = {
    github: 'https://github.com/settings/tokens/new',
    gitlab: 'https://gitlab.com/-/profile/personal_access_tokens',
  }

  info(`Visit: ${colors.primary(tokenUrl[provider])}\n`)

  const name = await promptText({
    message: 'Provider name',
    default: `${provider}-connection`,
    required: true,
  })

  const token = await promptPassword({
    message: 'Personal access token',
  })

  await withSpinner(`Connecting to ${provider}...`, async () => {
    if (provider === 'github') {
      const { error } = await createGithubPatProvider({
        client,
        body: { name, token },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
    } else if (provider === 'gitlab') {
      const baseUrl = await promptText({
        message: 'GitLab base URL (leave empty for gitlab.com)',
        default: '',
      })

      const { error } = await createGitlabPatProvider({
        client,
        body: {
          name,
          token,
          base_url: baseUrl || null,
        },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
    }
  })

  success(`${provider} connected successfully`)
}

async function removeProvider(providerId: string, options: { force?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(providerId, 10)
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
    warning(`Provider ${providerId} not found`)
    return
  }

  if (!options.force) {
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
    const { error } = await deleteProvider({
      client,
      path: { provider_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Provider removed')
}

async function showProvider(providerId: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(providerId, 10)
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
      throw new Error(getErrorMessage(error) ?? `Provider ${providerId} not found`)
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

async function connectGitProvider(provider: string): Promise<void> {
  if (provider !== 'github' && provider !== 'gitlab') {
    warning(`Unsupported provider: ${provider}. Supported: github, gitlab`)
    return
  }
  await addProvider()
}

async function listRepos(providerId?: string, options?: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const repos = await withSpinner('Fetching repositories...', async () => {
    if (providerId) {
      const id = parseInt(providerId, 10)
      if (isNaN(id)) {
        throw new Error('Invalid provider ID')
      }
      const { data, error } = await listRepositoriesByConnection({
        client,
        path: { connection_id: id },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data?.repositories ?? []
    } else {
      const { data, error } = await listSyncedRepositories({ client })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data?.repositories ?? []
    }
  })

  if (options?.json) {
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
    const visibility = repo.is_private ? colors.muted('(private)') : colors.success('(public)')
    console.log(`  ${colors.bold(repo.full_name)} ${visibility}`)
    console.log(`    ${colors.muted(`Branch: ${repo.default_branch}`)}`)
  }

  newline()
}
