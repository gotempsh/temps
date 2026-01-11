import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listProviders,
  createProvider,
  getProvider,
  deleteProvider,
  testProviderConnection,
  listProviderZones,
} from '../../api/sdk.gen.js'
import type { DnsProviderResponse, DnsProviderType } from '../../api/types.gen.js'
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
]

export function registerDnsCommands(program: Command): void {
  const dns = program
    .command('dns')
    .alias('dns-providers')
    .description('Manage DNS providers for automated domain verification')

  dns
    .command('list')
    .alias('ls')
    .description('List configured DNS providers')
    .option('--json', 'Output in JSON format')
    .action(listDnsProviders)

  dns
    .command('add')
    .description('Add a new DNS provider')
    .option('-t, --type <type>', 'Provider type (cloudflare, route53, digitalocean, namecheap, gcp, azure, manual)')
    .option('-n, --name <name>', 'Provider name')
    .option('-d, --description <description>', 'Provider description')
    // Cloudflare options
    .option('--api-token <token>', 'Cloudflare API token')
    .option('--account-id <id>', 'Cloudflare account ID (optional)')
    // Route53 options
    .option('--access-key-id <key>', 'AWS access key ID')
    .option('--secret-access-key <secret>', 'AWS secret access key')
    .option('--region <region>', 'AWS region')
    // DigitalOcean options (uses --api-token)
    // Namecheap options
    .option('--api-user <user>', 'Namecheap API user')
    .option('--api-key <key>', 'Namecheap API key')
    .option('--username <username>', 'Namecheap username')
    .option('--client-ip <ip>', 'Namecheap whitelisted client IP')
    // GCP options
    .option('--project-id <id>', 'GCP project ID')
    .option('--service-account-email <email>', 'GCP service account email')
    .option('--private-key-id <id>', 'GCP private key ID')
    .option('--private-key <key>', 'GCP private key')
    // Azure options
    .option('--tenant-id <id>', 'Azure tenant ID')
    .option('--client-id <id>', 'Azure client ID')
    .option('--client-secret <secret>', 'Azure client secret')
    .option('--subscription-id <id>', 'Azure subscription ID')
    .option('--resource-group <name>', 'Azure resource group')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(addProvider)

  dns
    .command('show')
    .description('Show DNS provider details')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showProvider)

  dns
    .command('remove')
    .alias('rm')
    .description('Remove a DNS provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(removeProvider)

  dns
    .command('test')
    .description('Test DNS provider connection')
    .requiredOption('--id <id>', 'Provider ID')
    .action(testProvider)

  dns
    .command('zones')
    .description('List available zones in a DNS provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(listZones)
}

async function listDnsProviders(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const providers = await withSpinner('Fetching DNS providers...', async () => {
    const { data, error } = await listProviders({ client })
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
    info('Run: temps dns add')
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

interface AddProviderOptions {
  type?: string
  name?: string
  description?: string
  // Cloudflare
  apiToken?: string
  accountId?: string
  // Route53
  accessKeyId?: string
  secretAccessKey?: string
  region?: string
  // Namecheap
  apiUser?: string
  apiKey?: string
  username?: string
  clientIp?: string
  // GCP
  projectId?: string
  serviceAccountEmail?: string
  privateKeyId?: string
  privateKey?: string
  // Azure
  tenantId?: string
  clientId?: string
  clientSecret?: string
  subscriptionId?: string
  resourceGroup?: string
  // Automation
  yes?: boolean
}

async function addProvider(options: AddProviderOptions): Promise<void> {
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

  let credentials: Record<string, unknown>

  switch (providerType) {
    case 'cloudflare': {
      let cfApiToken: string
      let cfAccountId: string | undefined

      if (options.apiToken) {
        cfApiToken = options.apiToken
        cfAccountId = options.accountId
      } else if (options.yes) {
        throw new Error('--api-token is required for Cloudflare when using --yes flag')
      } else {
        info('\nCloudflare DNS requires an API token with DNS:Edit permissions.')
        info('Create one at: https://dash.cloudflare.com/profile/api-tokens')
        newline()

        cfApiToken = await promptPassword({
          message: 'API Token',
        })

        cfAccountId = await promptText({
          message: 'Account ID (optional, for zone scoping)',
          default: '',
        })
      }

      credentials = {
        type: 'cloudflare',
        api_token: cfApiToken,
        ...(cfAccountId && { account_id: cfAccountId }),
      }
      break
    }

    case 'route53': {
      let awsAccessKey: string
      let awsSecretKey: string
      let awsRegion: string

      if (options.accessKeyId && options.secretAccessKey) {
        awsAccessKey = options.accessKeyId
        awsSecretKey = options.secretAccessKey
        awsRegion = options.region || 'us-east-1'
      } else if (options.yes) {
        throw new Error('--access-key-id and --secret-access-key are required for Route53 when using --yes flag')
      } else {
        info('\nAWS Route53 requires IAM credentials with Route53 permissions.')
        newline()

        awsAccessKey = await promptPassword({
          message: 'AWS Access Key ID',
        })

        awsSecretKey = await promptPassword({
          message: 'AWS Secret Access Key',
        })

        awsRegion = await promptText({
          message: 'AWS Region',
          default: 'us-east-1',
        })
      }

      credentials = {
        type: 'route53',
        access_key_id: awsAccessKey,
        secret_access_key: awsSecretKey,
        region: awsRegion,
      }
      break
    }

    case 'digitalocean': {
      let doApiToken: string

      if (options.apiToken) {
        doApiToken = options.apiToken
      } else if (options.yes) {
        throw new Error('--api-token is required for DigitalOcean when using --yes flag')
      } else {
        info('\nDigitalOcean requires a Personal Access Token.')
        info('Create one at: https://cloud.digitalocean.com/account/api/tokens')
        newline()

        doApiToken = await promptPassword({
          message: 'API Token',
        })
      }

      credentials = {
        type: 'digitalocean',
        api_token: doApiToken,
      }
      break
    }

    case 'namecheap': {
      let ncApiUser: string
      let ncApiKey: string
      let ncUsername: string
      let ncClientIp: string

      if (options.apiUser && options.apiKey && options.username && options.clientIp) {
        ncApiUser = options.apiUser
        ncApiKey = options.apiKey
        ncUsername = options.username
        ncClientIp = options.clientIp
      } else if (options.yes) {
        throw new Error('--api-user, --api-key, --username, and --client-ip are required for Namecheap when using --yes flag')
      } else {
        info('\nNamecheap requires API credentials.')
        info('Enable API access at: https://ap.www.namecheap.com/settings/tools/apiaccess/')
        newline()

        ncApiUser = await promptText({
          message: 'API User',
          required: true,
        })

        ncApiKey = await promptPassword({
          message: 'API Key',
        })

        ncUsername = await promptText({
          message: 'Username',
          required: true,
        })

        ncClientIp = await promptText({
          message: 'Client IP (whitelisted IP)',
          required: true,
        })
      }

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
      let gcpProject: string
      let gcpServiceAccountEmail: string
      let gcpPrivateKeyId: string
      let gcpPrivateKey: string

      if (options.projectId && options.serviceAccountEmail && options.privateKeyId && options.privateKey) {
        gcpProject = options.projectId
        gcpServiceAccountEmail = options.serviceAccountEmail
        gcpPrivateKeyId = options.privateKeyId
        gcpPrivateKey = options.privateKey
      } else if (options.yes) {
        throw new Error('--project-id, --service-account-email, --private-key-id, and --private-key are required for GCP when using --yes flag')
      } else {
        info('\nGoogle Cloud DNS requires a service account JSON key.')
        info('Create one at: https://console.cloud.google.com/iam-admin/serviceaccounts')
        newline()

        gcpProject = await promptText({
          message: 'Project ID',
          required: true,
        })

        gcpServiceAccountEmail = await promptText({
          message: 'Service Account Email',
          required: true,
        })

        gcpPrivateKeyId = await promptText({
          message: 'Private Key ID',
          required: true,
        })

        gcpPrivateKey = await promptPassword({
          message: 'Private Key (paste full key including BEGIN/END)',
        })
      }

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
      let azTenantId: string
      let azClientId: string
      let azClientSecret: string
      let azSubscriptionId: string
      let azResourceGroup: string

      if (options.tenantId && options.clientId && options.clientSecret && options.subscriptionId && options.resourceGroup) {
        azTenantId = options.tenantId
        azClientId = options.clientId
        azClientSecret = options.clientSecret
        azSubscriptionId = options.subscriptionId
        azResourceGroup = options.resourceGroup
      } else if (options.yes) {
        throw new Error('--tenant-id, --client-id, --client-secret, --subscription-id, and --resource-group are required for Azure when using --yes flag')
      } else {
        info('\nAzure DNS requires a service principal.')
        info('Create one with: az ad sp create-for-rbac --name "dns-provider"')
        newline()

        azTenantId = await promptText({
          message: 'Tenant ID',
          required: true,
        })

        azClientId = await promptText({
          message: 'Client ID',
          required: true,
        })

        azClientSecret = await promptPassword({
          message: 'Client Secret',
        })

        azSubscriptionId = await promptText({
          message: 'Subscription ID',
          required: true,
        })

        azResourceGroup = await promptText({
          message: 'Resource Group',
          required: true,
        })
      }

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

    case 'manual':
      if (!options.yes) {
        info('\nManual mode: You will need to create DNS records manually.')
      }
      credentials = {
        type: 'manual',
      }
      break

    default:
      throw new Error(`Unsupported provider type: ${providerType}`)
  }

  await withSpinner(`Creating ${providerType} DNS provider...`, async () => {
    const { error } = await createProvider({
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

  success(`${providerType} DNS provider created successfully`)
  info('Run: temps dns test --id <id> to verify the connection')
}

async function showProvider(options: { id: string; json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const provider = await withSpinner('Fetching provider...', async () => {
    const { data, error } = await getProvider({
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
  keyValue('Created', new Date(provider.created_at).toLocaleString())
  keyValue('Updated', new Date(provider.updated_at).toLocaleString())
  newline()
}

async function removeProvider(options: { id: string; force?: boolean; yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Get provider details first
  const { data: provider, error: getError } = await getProvider({
    client,
    path: { id },
  })

  if (getError || !provider) {
    warning(`Provider ${options.id} not found`)
    return
  }

  // Support both --force and --yes for skipping confirmation
  if (!options.force && !options.yes) {
    const confirmed = await promptConfirm({
      message: `Remove DNS provider "${provider.name}" (${provider.provider_type})?`,
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
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('DNS provider removed')
}

async function testProvider(options: { id: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  await withSpinner('Testing provider connection...', async () => {
    const { error } = await testProviderConnection({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Connection test successful!')
  info('The provider can connect to the DNS service')
}

async function listZones(options: { id: string; json?: boolean }): Promise<void> {
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
