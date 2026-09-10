// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listDnsProviders,
  createDnsProvider,
  getDnsProvider,
  updateProvider as updateDnsProvider,
  deleteDnsProvider,
  testProviderConnection,
  listProviderZones,
  listManagedDomains,
  addManagedDomain,
  removeManagedDomain,
  verifyManagedDomain,
  lookupDnsARecords,
} from '../../api/sdk.gen.js'
import type { DnsProviderResponse, DnsProviderType, ManagedDomainResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

const PROVIDER_TYPES: { name: string; value: DnsProviderType }[] = [
  { name: 'Cloudflare', value: 'cloudflare' },
  { name: 'Namecheap', value: 'namecheap' },
  { name: 'AWS Route53', value: 'route53' },
  { name: 'DigitalOcean', value: 'digitalocean' },
  { name: 'Google Cloud DNS', value: 'gcp' },
  { name: 'Azure DNS', value: 'azure' },
  { name: 'Manual', value: 'manual' },
  { name: 'Pebble (local ACME test server only)', value: 'pebble' },
]

// --- Option interfaces ---

interface ListOptions {
  json?: boolean
}

interface CreateOptions {
  name?: string
  type?: string
  description?: string
  apiToken?: string
  accountId?: string
  accessKeyId?: string
  secretAccessKey?: string
  region?: string
  apiUser?: string
  apiKey?: string
  username?: string
  clientIp?: string
  projectId?: string
  serviceAccountEmail?: string
  privateKeyId?: string
  privateKey?: string
  tenantId?: string
  clientId?: string
  clientSecret?: string
  subscriptionId?: string
  resourceGroup?: string
  managementUrl?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface UpdateOptions {
  id: string
  name?: string
  description?: string
  apiKey?: string
  active?: string
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface TestOptions {
  id: string
}

interface ZonesOptions {
  id: string
  json?: boolean
}

interface DomainsListOptions {
  id: string
  json?: boolean
}

interface DomainsAddOptions {
  id: string
  domain: string
  autoManage?: boolean
}

interface DomainsRemoveOptions {
  providerId: string
  domain: string
  force?: boolean
  yes?: boolean
}

interface DomainsVerifyOptions {
  providerId: string
  domain: string
}

interface LookupOptions {
  domain: string
  json?: boolean
}

// --- Credential resolution ---

// Maps flags already supplied on the command line to the credentials payload
// the API expects. Returns undefined when required flags are missing, which
// tells the caller to fall back to interactive prompts — unless --yes was
// passed, in which case automation has no prompts to fall back to and this
// throws the same actionable error the interactive path would have avoided.
export function resolveDnsProviderCredentials(
  providerType: DnsProviderType,
  options: CreateOptions,
): Record<string, unknown> | undefined {
  switch (providerType) {
    case 'cloudflare': {
      if (options.apiToken) {
        return {
          type: 'cloudflare',
          api_token: options.apiToken,
          ...(options.accountId && { account_id: options.accountId }),
        }
      }
      if (options.yes) {
        throw new Error('--api-token is required for Cloudflare when using --yes flag')
      }
      return undefined
    }

    case 'route53': {
      if (options.accessKeyId && options.secretAccessKey) {
        return {
          type: 'route53',
          access_key_id: options.accessKeyId,
          secret_access_key: options.secretAccessKey,
          region: options.region || 'us-east-1',
        }
      }
      if (options.yes) {
        throw new Error('--access-key-id and --secret-access-key are required for Route53 when using --yes flag')
      }
      return undefined
    }

    case 'digitalocean': {
      if (options.apiToken) {
        return { type: 'digitalocean', api_token: options.apiToken }
      }
      if (options.yes) {
        throw new Error('--api-token is required for DigitalOcean when using --yes flag')
      }
      return undefined
    }

    case 'namecheap': {
      if (options.apiUser && options.apiKey && options.username && options.clientIp) {
        return {
          type: 'namecheap',
          api_user: options.apiUser,
          api_key: options.apiKey,
          username: options.username,
          client_ip: options.clientIp,
        }
      }
      if (options.yes) {
        throw new Error('--api-user, --api-key, --username, and --client-ip are required for Namecheap when using --yes flag')
      }
      return undefined
    }

    case 'gcp': {
      if (options.projectId && options.serviceAccountEmail && options.privateKeyId && options.privateKey) {
        return {
          type: 'gcp',
          project_id: options.projectId,
          service_account_email: options.serviceAccountEmail,
          private_key_id: options.privateKeyId,
          private_key: options.privateKey,
        }
      }
      if (options.yes) {
        throw new Error('--project-id, --service-account-email, --private-key-id, and --private-key are required for GCP when using --yes flag')
      }
      return undefined
    }

    case 'azure': {
      if (options.tenantId && options.clientId && options.clientSecret && options.subscriptionId && options.resourceGroup) {
        return {
          type: 'azure',
          tenant_id: options.tenantId,
          client_id: options.clientId,
          client_secret: options.clientSecret,
          subscription_id: options.subscriptionId,
          resource_group: options.resourceGroup,
        }
      }
      if (options.yes) {
        throw new Error('--tenant-id, --client-id, --client-secret, --subscription-id, and --resource-group are required for Azure when using --yes flag')
      }
      return undefined
    }

    case 'manual':
      return { type: 'manual' }

    case 'pebble': {
      if (options.managementUrl) {
        return { type: 'pebble', management_url: options.managementUrl }
      }
      if (options.yes) {
        throw new Error('--management-url is required for Pebble when using --yes flag')
      }
      return undefined
    }

    default:
      throw new Error(`Unsupported provider type: ${providerType}`)
  }
}

// --- Command registration ---

export function registerDnsProvidersCommands(program: Command): void {
  const dnsProviders = program
    .command('dns-provider')
    .alias('dnsp')
    .description('Manage DNS providers and managed domains')

  dnsProviders
    .command('list')
    .alias('ls')
    .description('List all DNS providers')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  dnsProviders
    .command('create')
    .alias('add')
    .description('Create a new DNS provider')
    .option('-n, --name <name>', 'Provider name')
    .option('-t, --type <type>', 'Provider type (cloudflare, route53, digitalocean, namecheap, gcp, azure, manual, pebble)')
    .option('-d, --description <description>', 'Provider description')
    .option('--api-token <token>', 'API token (Cloudflare, DigitalOcean)')
    .option('--account-id <id>', 'Cloudflare account ID (optional)')
    .option('--access-key-id <key>', 'AWS access key ID')
    .option('--secret-access-key <secret>', 'AWS secret access key')
    .option('--region <region>', 'AWS region')
    .option('--api-user <user>', 'Namecheap API user')
    .option('--api-key <key>', 'Namecheap API key')
    .option('--username <username>', 'Namecheap username')
    .option('--client-ip <ip>', 'Namecheap whitelisted client IP')
    .option('--project-id <id>', 'GCP project ID')
    .option('--service-account-email <email>', 'GCP service account email')
    .option('--private-key-id <id>', 'GCP private key ID')
    .option('--private-key <key>', 'GCP private key')
    .option('--tenant-id <id>', 'Azure tenant ID')
    .option('--client-id <id>', 'Azure client ID')
    .option('--client-secret <secret>', 'Azure client secret')
    .option('--subscription-id <id>', 'Azure subscription ID')
    .option('--resource-group <name>', 'Azure resource group')
    .option('--management-url <url>', 'pebble-challtestsrv management API URL (local ACME test server only)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createAction)

  dnsProviders
    .command('show')
    .description('Show DNS provider details')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showAction)

  dnsProviders
    .command('update')
    .description('Update a DNS provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-n, --name <name>', 'New provider name')
    .option('-d, --description <description>', 'New description')
    .option('--api-key <key>', 'New API key/token')
    .option('--active <boolean>', 'Set active status (true/false)')
    .action(updateAction)

  dnsProviders
    .command('remove')
    .alias('rm')
    .description('Delete a DNS provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeAction)

  dnsProviders
    .command('test')
    .description('Test DNS provider connection')
    .requiredOption('--id <id>', 'Provider ID')
    .action(testAction)

  dnsProviders
    .command('zones')
    .description('List DNS zones for a provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(zonesAction)

  // Domains subcommand group
  const domains = dnsProviders
    .command('domains')
    .description('Manage domains associated with a DNS provider')

  domains
    .command('list')
    .alias('ls')
    .description('List managed domains for a provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(domainsListAction)

  domains
    .command('add')
    .description('Add a managed domain to a provider')
    .requiredOption('--id <id>', 'Provider ID')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('--auto-manage', 'Enable auto-management for DNS records')
    .action(domainsAddAction)

  domains
    .command('remove')
    .alias('rm')
    .description('Remove a managed domain from a provider')
    .requiredOption('--provider-id <id>', 'Provider ID')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(domainsRemoveAction)

  domains
    .command('verify')
    .description('Verify a managed domain')
    .requiredOption('--provider-id <id>', 'Provider ID')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .action(domainsVerifyAction)

  dnsProviders
    .command('lookup')
    .description('Lookup DNS A records for a domain')
    .requiredOption('-d, --domain <domain>', 'Domain name to lookup')
    .option('--json', 'Output in JSON format')
    .action(lookupAction)
}

// --- Action implementations ---

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const providers = await withSpinner('Fetching DNS providers...', async () => {
    const { data, error } = await listDnsProviders({ client })
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
  header(`${icons.info} DNS Providers (${providers.length})`)

  if (providers.length === 0) {
    info('No DNS providers configured')
    info('Run: temps dns-providers create')
    newline()
    return
  }

  const columns: TableColumn<DnsProviderResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'provider_type' },
    { header: 'Status', accessor: (p) => p.is_active ? 'enabled' : 'disabled', color: (v) => statusBadge(v === 'enabled' ? 'active' : 'inactive') },
    { header: 'Created', accessor: (p) => new Date(p.created_at).toLocaleDateString() },
  ]

  printTable(providers, columns, { style: 'minimal' })
  newline()
}

async function createAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Get provider type from flag or prompt
  let providerType: DnsProviderType
  if (options.type) {
    providerType = options.type as DnsProviderType
  } else if (options.yes) {
    throw new Error('--type is required when using --yes flag')
  } else {
    providerType = await promptSelect({
      message: 'DNS provider type',
      choices: PROVIDER_TYPES,
    }) as DnsProviderType
  }

  // Get name from flag or prompt
  let name: string
  if (options.name) {
    name = options.name
  } else if (options.yes) {
    name = `${providerType}-dns`
  } else {
    name = await promptText({
      message: 'Provider name',
      default: `${providerType}-dns`,
      required: true,
    })
  }

  // Get description from flag or prompt
  let description: string | undefined
  if (options.description !== undefined) {
    description = options.description
  } else if (!options.yes) {
    description = await promptText({
      message: 'Description (optional)',
      default: '',
    })
  }

  if (providerType === 'manual' && !options.yes) {
    info('\nManual mode: You will need to create DNS records manually.')
  }
  if (providerType === 'pebble' && !options.yes) {
    info(
      '\nPebble is a local ACME test server (pebble-challtestsrv) — never point this at a real DNS provider.',
    )
  }

  let credentials: Record<string, unknown> | undefined = resolveDnsProviderCredentials(providerType, options)

  if (!credentials) {
    switch (providerType) {
      case 'cloudflare': {
        info('\nCloudflare DNS requires an API token with DNS:Edit permissions.')
        info('Create one at: https://dash.cloudflare.com/profile/api-tokens')
        newline()

        const cfApiToken = await promptPassword({
          message: 'API Token',
        })

        const cfAccountId = await promptText({
          message: 'Account ID (optional, for zone scoping)',
          default: '',
        })

        credentials = {
          type: 'cloudflare',
          api_token: cfApiToken,
          ...(cfAccountId && { account_id: cfAccountId }),
        }
        break
      }

      case 'route53': {
        info('\nAWS Route53 requires IAM credentials with Route53 permissions.')
        newline()

        const awsAccessKey = await promptPassword({
          message: 'AWS Access Key ID',
        })

        const awsSecretKey = await promptPassword({
          message: 'AWS Secret Access Key',
        })

        const awsRegion = await promptText({
          message: 'AWS Region',
          default: 'us-east-1',
        })

        credentials = {
          type: 'route53',
          access_key_id: awsAccessKey,
          secret_access_key: awsSecretKey,
          region: awsRegion,
        }
        break
      }

      case 'digitalocean': {
        info('\nDigitalOcean requires a Personal Access Token.')
        info('Create one at: https://cloud.digitalocean.com/account/api/tokens')
        newline()

        const doApiToken = await promptPassword({
          message: 'API Token',
        })

        credentials = {
          type: 'digitalocean',
          api_token: doApiToken,
        }
        break
      }

      case 'namecheap': {
        info('\nNamecheap requires API credentials.')
        info('Enable API access at: https://ap.www.namecheap.com/settings/tools/apiaccess/')
        newline()

        const ncApiUser = await promptText({
          message: 'API User',
          required: true,
        })

        const ncApiKey = await promptPassword({
          message: 'API Key',
        })

        const ncUsername = await promptText({
          message: 'Username',
          required: true,
        })

        const ncClientIp = await promptText({
          message: 'Client IP (whitelisted IP)',
          required: true,
        })

        credentials = {
          type: 'namecheap',
          api_user: ncApiUser,
          api_key: ncApiKey,
          username: ncUsername,
          client_ip: ncClientIp,
        }
        break
      }

      case 'gcp': {
        info('\nGoogle Cloud DNS requires a service account JSON key.')
        info('Create one at: https://console.cloud.google.com/iam-admin/serviceaccounts')
        newline()

        const gcpProject = await promptText({
          message: 'Project ID',
          required: true,
        })

        const gcpServiceAccountEmail = await promptText({
          message: 'Service Account Email',
          required: true,
        })

        const gcpPrivateKeyId = await promptText({
          message: 'Private Key ID',
          required: true,
        })

        const gcpPrivateKey = await promptPassword({
          message: 'Private Key (paste full key including BEGIN/END)',
        })

        credentials = {
          type: 'gcp',
          project_id: gcpProject,
          service_account_email: gcpServiceAccountEmail,
          private_key_id: gcpPrivateKeyId,
          private_key: gcpPrivateKey,
        }
        break
      }

      case 'azure': {
        info('\nAzure DNS requires a service principal.')
        info('Create one with: az ad sp create-for-rbac --name "dns-provider"')
        newline()

        const azTenantId = await promptText({
          message: 'Tenant ID',
          required: true,
        })

        const azClientId = await promptText({
          message: 'Client ID',
          required: true,
        })

        const azClientSecret = await promptPassword({
          message: 'Client Secret',
        })

        const azSubscriptionId = await promptText({
          message: 'Subscription ID',
          required: true,
        })

        const azResourceGroup = await promptText({
          message: 'Resource Group',
          required: true,
        })

        credentials = {
          type: 'azure',
          tenant_id: azTenantId,
          client_id: azClientId,
          client_secret: azClientSecret,
          subscription_id: azSubscriptionId,
          resource_group: azResourceGroup,
        }
        break
      }

      case 'pebble': {
        const pebbleManagementUrl = await promptText({
          message: 'pebble-challtestsrv management API URL',
          required: true,
          default: 'http://localhost:8055',
        })

        credentials = {
          type: 'pebble',
          management_url: pebbleManagementUrl,
        }
        break
      }

      default:
        throw new Error(`Unsupported provider type: ${providerType}`)
    }
  }

  await withSpinner(`Creating ${providerType} DNS provider...`, async () => {
    const { error } = await createDnsProvider({
      client,
      body: {
        name,
        provider_type: providerType,
        credentials: credentials as never,
        ...(description && { description }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`${providerType} DNS provider "${name}" created successfully`)
  info('Run: temps dns-providers test --id <id> to verify the connection')
}

async function showAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const provider = await withSpinner('Fetching provider...', async () => {
    const { data, error } = await getDnsProvider({
      client,
      path: { id },
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
  header(`${icons.info} ${provider.name}`)
  keyValue('ID', provider.id)
  keyValue('Type', provider.provider_type)
  keyValue('Status', provider.is_active ? colors.success('enabled') : colors.muted('disabled'))
  if (provider.description) {
    keyValue('Description', provider.description)
  }
  if (provider.last_error) {
    keyValue('Last Error', colors.error(provider.last_error))
  }
  if (provider.last_used_at) {
    keyValue('Last Used', new Date(provider.last_used_at).toLocaleString())
  }
  keyValue('Created', new Date(provider.created_at).toLocaleString())
  keyValue('Updated', new Date(provider.updated_at).toLocaleString())
  newline()
}

async function updateAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const body: Record<string, unknown> = {}

  if (options.name) {
    body.name = options.name
  }

  if (options.description !== undefined) {
    body.description = options.description
  }

  if (options.apiKey) {
    body.credentials = { api_token: options.apiKey }
  }

  if (options.active !== undefined) {
    body.is_active = options.active === 'true'
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to update. Provide at least one of --name, --description, --api-key, or --active')
    return
  }

  const provider = await withSpinner('Updating DNS provider...', async () => {
    const { data, error } = await updateDnsProvider({
      client,
      path: { id },
      body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`DNS provider #${id} updated`)

  if (options.name) {
    info(`Name: ${options.name}`)
  }
  if (options.description !== undefined) {
    info(`Description: ${options.description || '(cleared)'}`)
  }
  if (options.apiKey) {
    info('Credentials: updated')
  }
  if (options.active !== undefined) {
    info(`Active: ${options.active}`)
  }
}

async function removeAction(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Get provider details first
  const { data: provider, error: getError } = await getDnsProvider({
    client,
    path: { id },
  })

  if (getError || !provider) {
    warning(`Provider ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove DNS provider "${provider.name}" (${provider.provider_type})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing DNS provider...', async () => {
    const { error } = await deleteDnsProvider({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('DNS provider removed')
}

async function testAction(options: TestOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const result = await withSpinner('Testing provider connection...', async () => {
    const { data, error } = await testProviderConnection({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result?.success) {
    success('Connection test successful!')
    info(result.message || 'The provider can connect to the DNS service')
  } else {
    warning('Connection test failed')
    if (result?.message) {
      info(`Message: ${result.message}`)
    }
  }
}

async function zonesAction(options: ZonesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const zones = await withSpinner('Fetching zones...', async () => {
    const { data, error } = await listProviderZones({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data?.zones ?? []
  })

  if (options.json) {
    json(zones)
    return
  }

  newline()
  header(`${icons.info} Available Zones (${zones.length})`)

  if (zones.length === 0) {
    info('No zones found in this provider')
    newline()
    return
  }

  for (const zone of zones) {
    console.log(`  ${colors.bold(zone.name)} ${colors.muted(`(${zone.id})`)}`)
    if (zone.status) {
      console.log(`    Status: ${statusBadge(zone.status === 'active' ? 'active' : 'inactive')}`)
    }
    if (zone.nameservers && zone.nameservers.length > 0) {
      console.log(`    Nameservers: ${colors.muted(zone.nameservers.join(', '))}`)
    }
  }
  newline()
}

async function domainsListAction(options: DomainsListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const domainsData = await withSpinner('Fetching managed domains...', async () => {
    const { data, error } = await listManagedDomains({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(domainsData)
    return
  }

  newline()
  header(`${icons.info} Managed Domains (${domainsData.length})`)

  if (domainsData.length === 0) {
    info('No managed domains for this provider')
    info(`Run: temps dns-providers domains add --id ${id} --domain example.com`)
    newline()
    return
  }

  const columns: TableColumn<ManagedDomainResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Verified', accessor: (d) => d.verified ? 'yes' : 'no', color: (v) => statusBadge(v === 'yes' ? 'active' : 'inactive') },
    { header: 'Auto-Manage', accessor: (d) => d.auto_manage ? 'yes' : 'no' },
    { header: 'Zone', accessor: (d) => d.zone_id || '-', color: (v) => colors.muted(v) },
    { header: 'Verified At', accessor: (d) => d.verified_at ? new Date(d.verified_at).toLocaleDateString() : '-' },
  ]

  printTable(domainsData, columns, { style: 'minimal' })
  newline()
}

async function domainsAddAction(options: DomainsAddOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const result = await withSpinner(`Adding domain ${options.domain}...`, async () => {
    const { data, error } = await addManagedDomain({
      client,
      path: { id },
      body: {
        domain: options.domain,
        ...(options.autoManage !== undefined && { auto_manage: options.autoManage }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Domain "${options.domain}" added to provider #${id}`)

  if (result && !result.verified) {
    info(`Run: temps dns-providers domains verify --provider-id ${id} --domain ${options.domain}`)
  }
}

async function domainsRemoveAction(options: DomainsRemoveOptions): Promise<void> {
  await requireAuth()

  const providerId = parseInt(options.providerId, 10)
  if (isNaN(providerId)) {
    warning('Invalid provider ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove managed domain "${options.domain}" from provider #${options.providerId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await setupClient()

  await withSpinner(`Removing domain ${options.domain}...`, async () => {
    const { error } = await removeManagedDomain({
      client,
      path: { provider_id: providerId, domain: options.domain },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Domain "${options.domain}" removed from provider #${providerId}`)
}

async function domainsVerifyAction(options: DomainsVerifyOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const providerId = parseInt(options.providerId, 10)
  if (isNaN(providerId)) {
    warning('Invalid provider ID')
    return
  }

  const result = await withSpinner(`Verifying domain ${options.domain}...`, async () => {
    const { data, error } = await verifyManagedDomain({
      client,
      path: { provider_id: providerId, domain: options.domain },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result?.verified) {
    success(`Domain "${options.domain}" verified successfully`)
    if (result.verified_at) {
      info(`Verified at: ${new Date(result.verified_at).toLocaleString()}`)
    }
  } else {
    warning(`Domain "${options.domain}" could not be verified`)
    if (result?.verification_error) {
      info(`Error: ${result.verification_error}`)
    }
  }
}

async function lookupAction(options: LookupOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner(`Looking up DNS A records for ${options.domain}...`, async () => {
    const { data, error } = await lookupDnsARecords({
      client,
      query: { domain: options.domain },
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
  header(`${icons.info} DNS A Records for ${options.domain}`)

  if (!result || result.records.length === 0) {
    info('No A records found')
    newline()
    return
  }

  keyValue('Domain', result.domain)
  keyValue('Records Found', result.count)
  keyValue('DNS Servers', result.dns_servers.join(', '))

  newline()
  header('A Records')
  for (const record of result.records) {
    console.log(`  ${colors.bold(record)}`)
  }
  newline()
}
