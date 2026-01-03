import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listDomains as listDomainsApi,
  createDomain,
  deleteDomain,
  provisionDomain,
  renewDomain,
  checkDomainStatus,
} from '../../api/sdk.gen.js'
import type { DomainResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, warning, info,
  keyValue, formatDate, box
} from '../../ui/output.js'

// Helper function to find domain ID by domain name
async function findDomainIdByName(domainName: string): Promise<number | null> {
  const { data, error } = await listDomainsApi({ client })
  if (error || !data?.domains) return null

  const domain = data.domains.find((d: DomainResponse) => d.domain === domainName)
  return domain?.id ?? null
}

interface AddOptions {
  domain: string
  challenge?: string
}

interface VerifyOptions {
  domain: string
}

interface RemoveOptions {
  domain: string
  force?: boolean
  yes?: boolean
}

interface SslOptions {
  domain: string
  renew?: boolean
}

interface StatusOptions {
  domain: string
}

export function registerDomainsCommands(program: Command): void {
  const domains = program
    .command('domains')
    .alias('domain')
    .description('Manage custom domains')

  domains
    .command('list')
    .alias('ls')
    .description('List domains')
    .option('--json', 'Output in JSON format')
    .action(listDomains)

  domains
    .command('add')
    .description('Add a custom domain')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('-c, --challenge <type>', 'Challenge type (http-01 or dns-01)', 'http-01')
    .action(addDomain)

  domains
    .command('verify')
    .description('Verify domain and provision SSL certificate')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .action(verifyDomain)

  domains
    .command('remove')
    .alias('rm')
    .description('Remove a domain')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeDomain)

  domains
    .command('ssl')
    .description('Manage SSL certificate')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('--renew', 'Force certificate renewal')
    .action(manageSsl)

  domains
    .command('status')
    .description('Check domain status')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .action(domainStatus)
}

async function listDomains(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const domains = await withSpinner('Fetching domains...', async () => {
    const { data, error } = await listDomainsApi({ client })
    if (error) throw new Error(getErrorMessage(error))
    return data?.domains ?? []
  })

  if (options.json) {
    json(domains)
    return
  }

  newline()
  header(`${icons.globe} Domains (${domains.length})`)

  const columns: TableColumn<DomainResponse>[] = [
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Status', accessor: (d) => d.status, color: (v) => statusBadge(v) },
    { header: 'Wildcard', accessor: (d) => d.is_wildcard ? 'Yes' : 'No' },
    { header: 'Method', accessor: (d) => d.verification_method },
    {
      header: 'Expires',
      accessor: (d) => d.expiration_time ? formatDate(new Date(d.expiration_time * 1000).toISOString()) : '-',
      color: (v) => colors.muted(v)
    },
  ]

  printTable(domains, columns, { style: 'minimal' })
  newline()
}

async function addDomain(options: AddOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domain = options.domain

  newline()
  info(`Adding domain ${colors.bold(domain)}`)

  const result = await withSpinner('Adding domain...', async () => {
    const { data, error } = await createDomain({
      client,
      body: {
        domain,
        challenge_type: options.challenge || 'http-01',
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  success(`Domain ${domain} added`)

  if (result?.dns_challenge_token && result?.dns_challenge_value) {
    newline()
    box(
      `Type: TXT\n` +
      `Name: ${result.dns_challenge_token}\n` +
      `Value: ${result.dns_challenge_value}`,
      'Add this DNS record to verify ownership'
    )
    newline()
    info(`Run "temps domains verify --domain ${domain}" after adding the record`)
  } else if (options.challenge === 'http-01') {
    newline()
    info('HTTP-01 challenge will be validated automatically when provisioning')
    info(`Run "temps domains verify --domain ${domain}" to provision SSL certificate`)
  }
}

async function verifyDomain(options: VerifyOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domain = options.domain

  const result = await withSpinner(`Provisioning SSL for ${domain}...`, async () => {
    const { data, error } = await provisionDomain({
      client,
      path: { domain },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  if (!result) {
    warning('No response received')
    return
  }

  // Handle union type based on 'type' discriminator
  if (result.type === 'complete') {
    const domainData = result
    if (domainData.status === 'active' || domainData.status === 'provisioned') {
      success(`Domain ${domain} verified and SSL certificate provisioned`)
    } else {
      warning(`Domain status: ${domainData.status}`)
      if (domainData.last_error) {
        warning(`Error: ${domainData.last_error}`)
      }
    }
  } else if (result.type === 'pending') {
    info(`Domain ${domain} is pending verification`)
    info('Please ensure DNS records are properly configured')
  } else if (result.type === 'error') {
    const errorData = result
    warning(`Domain provisioning error: ${errorData.message}`)
    if (errorData.details) {
      warning(`Details: ${errorData.details}`)
    }
  }
}

async function removeDomain(options: RemoveOptions): Promise<void> {
  await requireAuth()

  const domain = options.domain
  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove domain ${domain}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await setupClient()

  await withSpinner(`Removing ${domain}...`, async () => {
    const { error } = await deleteDomain({
      client,
      path: { domain },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Domain ${domain} removed`)
}

async function manageSsl(options: SslOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainName = options.domain

  if (options.renew) {
    await withSpinner(`Renewing SSL certificate for ${domainName}...`, async () => {
      const { error } = await renewDomain({
        client,
        path: { domain: domainName },
      })
      if (error) throw new Error(getErrorMessage(error))
    })
    success('SSL certificate renewal initiated')
    return
  }

  // Look up domain ID by name
  const domainId = await findDomainIdByName(domainName)
  if (!domainId) {
    warning(`Domain ${domainName} not found`)
    return
  }

  const sslInfo = await withSpinner('Fetching SSL info...', async () => {
    const { data, error } = await checkDomainStatus({
      client,
      path: { domain: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  header(`${icons.lock} SSL Certificate for ${domainName}`)
  keyValue('Status', statusBadge(sslInfo?.status ?? 'unknown'))
  keyValue('Wildcard', sslInfo?.is_wildcard ? 'Yes' : 'No')
  keyValue('Method', sslInfo?.verification_method ?? '-')
  keyValue('Expires', sslInfo?.expiration_time ? formatDate(new Date(sslInfo.expiration_time * 1000).toISOString()) : '-')
  if (sslInfo?.last_renewed) {
    keyValue('Last Renewed', formatDate(new Date(sslInfo.last_renewed * 1000).toISOString()))
  }
  if (sslInfo?.last_error) {
    keyValue('Last Error', colors.error(sslInfo.last_error))
  }
  newline()
}

async function domainStatus(options: StatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainName = options.domain

  // Look up domain ID by name
  const domainId = await findDomainIdByName(domainName)
  if (!domainId) {
    warning(`Domain ${domainName} not found`)
    return
  }

  const status = await withSpinner(`Checking status for ${domainName}...`, async () => {
    const { data, error } = await checkDomainStatus({
      client,
      path: { domain: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  header(`${icons.globe} Domain Status: ${domainName}`)
  keyValue('Status', statusBadge(status?.status ?? 'unknown'))
  keyValue('Wildcard', status?.is_wildcard ? 'Yes' : 'No')
  keyValue('Verification Method', status?.verification_method ?? '-')

  if (status?.dns_challenge_token) {
    keyValue('DNS Challenge Token', status.dns_challenge_token)
  }
  if (status?.dns_challenge_value) {
    keyValue('DNS Challenge Value', status.dns_challenge_value)
  }

  keyValue('Certificate Expires', status?.expiration_time ? formatDate(new Date(status.expiration_time * 1000).toISOString()) : '-')

  if (status?.last_error) {
    newline()
    warning(`Last Error: ${status.last_error}`)
    if (status.last_error_type) {
      keyValue('Error Type', status.last_error_type)
    }
  }

  newline()
}
