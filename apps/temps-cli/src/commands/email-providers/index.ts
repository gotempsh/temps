import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listEmailProviders as listEmailSmtpProviders,
  createEmailProvider as createEmailSmtpProvider,
  getEmailProvider as getEmailSmtpProvider,
  deleteEmailProvider as deleteEmailSmtpProvider,
  testProvider as testEmailProvider,
} from '../../api/sdk.gen.js'
import type {
  EmailProviderResponse,
  EmailProviderTypeRoute,
  ScalewayCredentialsRequest,
  SesCredentialsRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptSelect, promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning,
  keyValue, formatDate,
} from '../../ui/output.js'

const PROVIDER_TYPES: { name: string; value: EmailProviderTypeRoute }[] = [
  { name: 'Amazon SES', value: 'ses' },
  { name: 'Scaleway', value: 'scaleway' },
]

interface CreateOptions {
  name?: string
  type?: string
  region?: string
  accessKeyId?: string
  secretAccessKey?: string
  apiKey?: string
  projectId?: string
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

interface TestOptions {
  id: string
  from?: string
  fromName?: string
}

// --- Credential resolution ---

// Maps flags already supplied on the command line to the ses_credentials /
// scaleway_credentials pair the API expects (exactly one populated, the
// other null). Returns undefined when required flags are missing so the
// caller falls back to interactive prompts — unless --yes was passed, in
// which case automation has no prompts to fall back to and this throws the
// same actionable error the interactive path would have avoided.
export function resolveEmailProviderCredentials(
  providerType: EmailProviderTypeRoute,
  options: CreateOptions,
): { ses_credentials: SesCredentialsRequest | null; scaleway_credentials: ScalewayCredentialsRequest | null } | undefined {
  switch (providerType) {
    case 'ses': {
      if (options.accessKeyId && options.secretAccessKey) {
        return {
          ses_credentials: {
            access_key_id: options.accessKeyId,
            secret_access_key: options.secretAccessKey,
          },
          scaleway_credentials: null,
        }
      }
      if (options.yes) {
        throw new Error('--access-key-id and --secret-access-key are required for SES when using --yes flag')
      }
      return undefined
    }

    case 'scaleway': {
      if (options.apiKey && options.projectId) {
        return {
          ses_credentials: null,
          scaleway_credentials: {
            api_key: options.apiKey,
            project_id: options.projectId,
          },
        }
      }
      if (options.yes) {
        throw new Error('--api-key and --project-id are required for Scaleway when using --yes flag')
      }
      return undefined
    }

    default:
      return undefined
  }
}

export function registerEmailProvidersCommands(program: Command): void {
  const emailProviders = program
    .command('email-providers')
    .alias('eprov')
    .description('Manage email providers (SES, Scaleway) for transactional email')

  emailProviders
    .command('list')
    .alias('ls')
    .description('List all email providers')
    .option('--json', 'Output in JSON format')
    .action(listProvidersAction)

  emailProviders
    .command('create')
    .alias('add')
    .description('Create a new email provider')
    .option('-n, --name <name>', 'Provider name')
    .option('-t, --type <type>', 'Provider type (ses, scaleway)')
    .option('-r, --region <region>', 'Cloud region')
    // SES options
    .option('--access-key-id <key>', 'AWS access key ID (for SES)')
    .option('--secret-access-key <secret>', 'AWS secret access key (for SES)')
    // Scaleway options
    .option('--api-key <key>', 'Scaleway API key')
    .option('--project-id <id>', 'Scaleway project ID')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createProviderAction)

  emailProviders
    .command('show')
    .description('Show email provider details')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showProviderAction)

  emailProviders
    .command('remove')
    .alias('rm')
    .description('Remove an email provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeProviderAction)

  emailProviders
    .command('test')
    .description('Test an email provider by sending a test email')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--from <email>', 'Sender email address (must be verified)')
    .option('--from-name <name>', 'Sender display name')
    .action(testProviderAction)
}

async function listProvidersAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const providers = await withSpinner('Fetching email providers...', async () => {
    const { data, error } = await listEmailSmtpProviders({ client })
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
  header(`${icons.info} Email Providers (${providers.length})`)

  if (providers.length === 0) {
    info('No email providers configured')
    info('Run: temps email-providers create')
    newline()
    return
  }

  const columns: TableColumn<EmailProviderResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'provider_type' },
    { header: 'Region', key: 'region' },
    { header: 'Status', accessor: (p) => p.is_active ? 'enabled' : 'disabled', color: (v) => statusBadge(v === 'enabled' ? 'active' : 'inactive') },
    { header: 'Created', accessor: (p) => formatDate(p.created_at), color: (v) => colors.muted(v) },
  ]

  printTable(providers, columns, { style: 'minimal' })
  newline()
}

async function createProviderAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let providerType: EmailProviderTypeRoute
  let name: string
  let region: string

  // Check if automation mode
  const isAutomation = options.yes && options.type && options.name && options.region

  if (isAutomation) {
    providerType = options.type as EmailProviderTypeRoute
    name = options.name!
    region = options.region!

    if (providerType !== 'ses' && providerType !== 'scaleway') {
      warning(`Invalid provider type: ${providerType}. Supported: ses, scaleway`)
      return
    }
  } else {
    // Interactive mode
    providerType = options.type
      ? options.type as EmailProviderTypeRoute
      : await promptSelect({
          message: 'Email provider type',
          choices: PROVIDER_TYPES,
        }) as EmailProviderTypeRoute

    if (providerType !== 'ses' && providerType !== 'scaleway') {
      warning(`Invalid provider type: ${providerType}. Supported: ses, scaleway`)
      return
    }

    name = options.name || await promptText({
      message: 'Provider name',
      default: `${providerType}-email`,
      required: true,
    })

    region = options.region || await promptText({
      message: 'Cloud region',
      default: providerType === 'ses' ? 'us-east-1' : 'fr-par',
      required: true,
    })
  }

  // Build credentials based on type
  let sesCredentials: SesCredentialsRequest | null = null
  let scalewayCredentials: ScalewayCredentialsRequest | null = null

  const resolved = resolveEmailProviderCredentials(providerType, options)

  if (resolved) {
    sesCredentials = resolved.ses_credentials
    scalewayCredentials = resolved.scaleway_credentials
  } else {
    switch (providerType) {
      case 'ses': {
        info('\nAmazon SES requires IAM credentials with SES send permissions.')
        info('Create credentials at: https://console.aws.amazon.com/iam/')
        newline()

        const accessKeyId = await promptText({
          message: 'AWS Access Key ID',
          required: true,
        })

        const secretAccessKey = await promptText({
          message: 'AWS Secret Access Key',
          required: true,
        })

        sesCredentials = {
          access_key_id: accessKeyId,
          secret_access_key: secretAccessKey,
        }
        break
      }

      case 'scaleway': {
        info('\nScaleway Transactional Email requires API credentials.')
        info('Create credentials at: https://console.scaleway.com/iam/api-keys')
        newline()

        const apiKey = await promptText({
          message: 'Scaleway API Key',
          required: true,
        })

        const projectId = await promptText({
          message: 'Scaleway Project ID',
          required: true,
        })

        scalewayCredentials = {
          api_key: apiKey,
          project_id: projectId,
        }
        break
      }
    }
  }

  await withSpinner(`Creating ${providerType} email provider...`, async () => {
    const { error } = await createEmailSmtpProvider({
      client,
      body: {
        name,
        provider_type: providerType,
        region,
        ses_credentials: sesCredentials,
        scaleway_credentials: scalewayCredentials,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`${providerType} email provider created successfully`)
  info('Run: temps email-providers test --id <id> --from sender@yourdomain.com to send a test email')
}

async function showProviderAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const provider = await withSpinner('Fetching email provider...', async () => {
    const { data, error } = await getEmailSmtpProvider({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Email provider ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(provider)
    return
  }

  newline()
  header(`${icons.info} ${provider.name}`)
  keyValue('ID', provider.id)
  keyValue('Type', provider.provider_type)
  keyValue('Region', provider.region)
  keyValue('Status', provider.is_active ? colors.success('enabled') : colors.muted('disabled'))
  keyValue('Created', formatDate(provider.created_at))
  keyValue('Updated', formatDate(provider.updated_at))
  newline()
}

async function removeProviderAction(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Get provider details first
  const { data: provider, error: getError } = await getEmailSmtpProvider({
    client,
    path: { id },
  })

  if (getError || !provider) {
    warning(`Email provider ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove email provider "${provider.name}" (${provider.provider_type})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing email provider...', async () => {
    const { error } = await deleteEmailSmtpProvider({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Email provider removed')
}

async function testProviderAction(options: TestOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  let from: string
  let fromName: string | null = null

  if (options.from) {
    from = options.from
    fromName = options.fromName || null
  } else {
    from = await promptText({
      message: 'Sender email address (must be verified with the provider)',
      required: true,
    })

    fromName = await promptText({
      message: 'Sender display name (optional)',
      default: '',
    }) || null
  }

  await withSpinner('Sending test email...', async () => {
    const { error } = await testEmailProvider({
      client,
      path: { id },
      body: {
        from,
        ...(fromName && { from_name: fromName }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Test email sent successfully!')
  info('Check your inbox for the test message')
}
