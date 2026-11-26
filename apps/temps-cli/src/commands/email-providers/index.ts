// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listEmailProviders as listEmailSmtpProviders,
  createEmailProvider as createEmailSmtpProvider,
  getEmailProvider as getEmailSmtpProvider,
  deleteEmailProvider as deleteEmailSmtpProvider,
  testProvider as testEmailProvider,
  listDiscoverableDomains as listDiscoverableDomainsSdk,
} from '../../api/sdk.gen.js'
import type {
  EmailProviderResponse,
  EmailProviderTypeRoute,
  ProviderDomainIdentityResponse,
  ScalewayCredentialsRequest,
  SesCredentialsRequest,
  SmtpCredentialsRequest,
  SmtpEncryptionRoute,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning,
  keyValue, formatDate,
} from '../../ui/output.js'
import { addCredentialSources } from '../../lib/value-source.js'

const PROVIDER_TYPES: { name: string; value: EmailProviderTypeRoute }[] = [
  { name: 'Amazon SES', value: 'ses' },
  { name: 'Scaleway', value: 'scaleway' },
  { name: 'SMTP server', value: 'smtp' },
]

const SMTP_ENCRYPTION: SmtpEncryptionRoute[] = ['starttls', 'tls', 'none']

interface CreateOptions {
  name?: string
  type?: string
  region?: string
  accessKeyId?: string
  secretAccessKey?: string
  apiKey?: string
  projectId?: string
  smtpHost?: string
  smtpPort?: string
  smtpUsername?: string
  smtpPassword?: string
  smtpEncryption?: string
  smtpAcceptInvalidCerts?: boolean
  snsTopicArn?: string
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

interface DiscoverableDomainsOptions {
  id: string
  json?: boolean
}

// --- Credential resolution ---

/** Validator for a masked credential prompt: an empty answer is not a credential. */
export function requireNonEmpty(value: string): true | string {
  return value.length > 0 ? true : 'A value is required'
}

// Maps flags already supplied on the command line to the ses_credentials /
// scaleway_credentials pair the API expects (exactly one populated, the
// other null). Returns undefined when required flags are missing so the
// caller falls back to interactive prompts — unless --yes was passed, in
// which case automation has no prompts to fall back to and this throws the
// same actionable error the interactive path would have avoided.
export function resolveEmailProviderCredentials(
  providerType: EmailProviderTypeRoute,
  options: CreateOptions,
):
  | {
      ses_credentials: SesCredentialsRequest | null
      scaleway_credentials: ScalewayCredentialsRequest | null
      smtp_credentials?: SmtpCredentialsRequest | null
    }
  | undefined {
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

    case 'smtp':
      return { ses_credentials: null, scaleway_credentials: null, smtp_credentials: smtpCredentials(options) }

    default:
      return undefined
  }
}

/** SMTP settings from the flags; the host is required, the port defaults to the encryption's usual one. */
export function smtpCredentials(options: CreateOptions): SmtpCredentialsRequest {
  if (!options.smtpHost) throw new Error('--smtp-host is required for an SMTP provider')
  const encryption = (options.smtpEncryption ?? 'starttls') as SmtpEncryptionRoute
  if (!SMTP_ENCRYPTION.includes(encryption)) {
    throw new Error(`--smtp-encryption must be one of ${SMTP_ENCRYPTION.join(', ')}, got "${options.smtpEncryption}"`)
  }
  const port = options.smtpPort === undefined ? (encryption === 'tls' ? 465 : 587) : Number(options.smtpPort)
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`--smtp-port must be a port number (1-65535), got "${options.smtpPort}"`)
  }
  return {
    host: options.smtpHost,
    port,
    encryption,
    ...(options.smtpUsername && { username: options.smtpUsername }),
    ...(options.smtpPassword && { password: options.smtpPassword }),
    ...(options.smtpAcceptInvalidCerts && { accept_invalid_certs: true }),
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

  const createCommand = emailProviders
    .command('create')
    .alias('add')
    .description('Create a new email provider')
    .option('-n, --name <name>', 'Provider name')
    .option('-t, --type <type>', 'Provider type (ses, scaleway, smtp)')
    .option('-r, --region <region>', 'Cloud region')
    // SES options
    .option('--access-key-id <key>', 'AWS access key ID (for SES)')
    .option('--secret-access-key <secret>', 'AWS secret access key (for SES)')
    // Scaleway options
    .option('--api-key <key>', 'Scaleway API key')
    .option('--project-id <id>', 'Scaleway project ID')
    .option('--sns-topic-arn <arn>', 'SES: the only SNS topic allowed to deliver bounce and complaint events')
    // SMTP options
    .option('--smtp-host <host>', 'SMTP server host name')
    .option('--smtp-port <port>', 'SMTP port (default: 587, or 465 with --smtp-encryption tls)')
    .option('--smtp-encryption <mode>', 'starttls (default), tls or none')
    .option('--smtp-username <user>', 'SMTP user name, when the server requires a sign-in')
    .option('--smtp-password <password>', 'SMTP password')
    .option('--smtp-accept-invalid-certs', 'Accept a self-signed or otherwise invalid server certificate')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createProviderAction)
  addCredentialSources(createCommand, [
    { flag: 'secret-access-key', what: 'the AWS secret access key' },
    { flag: 'api-key', what: 'the Scaleway API key' },
    { flag: 'smtp-password', what: 'the SMTP password' },
  ])

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

  emailProviders
    .command('discoverable-domains')
    .alias('discover-domains')
    .description('List domain identities already registered on the provider\'s side, for importing')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(discoverableDomainsAction)
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
  // An SMTP provider's region is informational only: the host and port route the mail.
  const regionFlag = options.region ?? (options.type === 'smtp' ? 'smtp' : undefined)
  const isAutomation = options.yes && options.type && options.name && regionFlag

  if (isAutomation) {
    providerType = options.type as EmailProviderTypeRoute
    name = options.name!
    region = regionFlag!

    if (!PROVIDER_TYPES.some((t) => t.value === providerType)) {
      warning(`Invalid provider type: ${providerType}. Supported: ses, scaleway, smtp`)
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

    if (!PROVIDER_TYPES.some((t) => t.value === providerType)) {
      warning(`Invalid provider type: ${providerType}. Supported: ses, scaleway, smtp`)
      return
    }

    name = options.name || await promptText({
      message: 'Provider name',
      default: `${providerType}-email`,
      required: true,
    })

    region = regionFlag || await promptText({
      message: providerType === 'smtp' ? 'Label for this server (informational)' : 'Cloud region',
      default: providerType === 'ses' ? 'us-east-1' : providerType === 'smtp' ? 'smtp' : 'fr-par',
      required: true,
    })
  }

  // Build credentials based on type
  let sesCredentials: SesCredentialsRequest | null = null
  let scalewayCredentials: ScalewayCredentialsRequest | null = null
  let smtpCredentialsBody: SmtpCredentialsRequest | null = null

  const resolved = resolveEmailProviderCredentials(providerType, options)

  if (resolved) {
    sesCredentials = resolved.ses_credentials
    scalewayCredentials = resolved.scaleway_credentials
    smtpCredentialsBody = resolved.smtp_credentials ?? null
  } else {
    switch (providerType) {
      case 'ses': {
        info('\nAmazon SES requires IAM credentials with SES send permissions.')
        info('Create credentials at: https://console.aws.amazon.com/iam/')
        newline()

        // Keep what was already supplied (e.g. --secret-access-key-file) and
        // prompt only for what is missing (an empty flag counts as missing, as
        // in resolveEmailProviderCredentials); the secret is typed masked.
        const accessKeyId = options.accessKeyId || await promptText({
          message: 'AWS Access Key ID',
          required: true,
        })

        const secretAccessKey = options.secretAccessKey || await promptPassword({
          message: 'AWS Secret Access Key',
          validate: requireNonEmpty,
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

        const apiKey = options.apiKey || await promptPassword({
          message: 'Scaleway API Key',
          validate: requireNonEmpty,
        })

        const projectId = options.projectId || await promptText({
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
        smtp_credentials: smtpCredentialsBody,
        ...(options.snsTopicArn && { sns_topic_arn: options.snsTopicArn }),
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

async function discoverableDomainsAction(options: DiscoverableDomainsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const result = await withSpinner('Listing domains from provider...', async () => {
    const { data, error } = await listDiscoverableDomainsSdk({ client, path: { id } })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Failed to list domains for provider ${options.id}`)
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Discoverable Domains`)

  if (!result.supported) {
    info('This provider type does not support listing registered domains.')
    info('Use: temps email-domains import --provider-id <id> --domain <domain> to import one by name.')
    newline()
    return
  }

  if (result.error) {
    warning(`Could not fetch domains from the provider: ${result.error}`)
    info('Use: temps email-domains import --provider-id <id> --domain <domain> to import one by name.')
    newline()
    return
  }

  if (result.domains.length === 0) {
    info('No domains registered with this provider')
    newline()
    return
  }

  const columns: TableColumn<ProviderDomainIdentityResponse>[] = [
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v === 'verified' ? 'active' : 'inactive') },
    { header: 'Identity ID', key: 'provider_identity_id', color: (v) => colors.muted(v) },
  ]

  printTable(result.domains, columns, { style: 'minimal' })
  newline()
}
