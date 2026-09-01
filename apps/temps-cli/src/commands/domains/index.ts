// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
  listOrders as listOrdersApi,
  getDomainOrder,
  createOrRecreateOrder,
  finalizeOrder as finalizeOrderApi,
  cancelDomainOrder,
  setupDnsChallenge as setupDnsChallengeApi,
  getHttpChallengeDebug,
  listRenewalAttempts as listRenewalAttemptsApi,
} from '../../api/sdk.gen.js'
import type {
  DomainResponse,
  AcmeOrderResponse,
  RenewalAttemptResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, warning, info,
  keyValue, formatDate, box
} from '../../ui/output.js'

// Exact match only — a case-insensitive or partial match here would let
// `domains ssl`/`domains status` silently target the wrong domain.
export function findDomainByName(domains: DomainResponse[], domainName: string): number | null {
  const domain = domains.find((d) => d.domain === domainName)
  return domain?.id ?? null
}

// Helper function to find domain ID by domain name
async function findDomainIdByName(domainName: string): Promise<number | null> {
  const { data, error } = await listDomainsApi({ client })
  if (error || !data?.domains) return null

  return findDomainByName(data.domains, domainName)
}

// The ACME TXT record for a wildcard domain (`*.example.com`) is always
// scoped to the base domain, never the literal `*.` name.
export function wildcardBaseDomain(domain: string): string {
  return domain.startsWith('*.') ? domain.slice(2) : domain
}

export function isProvisionedStatus(status: string): boolean {
  return status === 'active' || status === 'provisioned'
}

interface AddOptions {
  domain: string
  challenge?: string
  yes?: boolean
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

interface RenewalAttemptsOptions {
  domain: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface OrderShowOptions {
  domainId: string
  json?: boolean
}

interface OrderCreateOptions {
  domainId: string
}

interface OrderFinalizeOptions {
  domainId: string
}

interface OrderCancelOptions {
  domainId: string
  force?: boolean
  yes?: boolean
}

interface DnsChallengeOptions {
  domainId: string
  providerId: string
}

interface HttpDebugOptions {
  domain: string
  json?: boolean
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
    .option('-y, --yes', 'Skip confirmation prompts')
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

  domains
    .command('renewal-attempts')
    .description('Show the certificate renewal-attempt history for a domain')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('--page <page>', 'Page number (1-indexed)', '1')
    .option('--page-size <pageSize>', 'Items per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(listRenewalAttempts)

  // --- Nested orders command group ---
  const orders = domains
    .command('orders')
    .alias('order')
    .description('Manage ACME orders for SSL certificate provisioning')

  orders
    .command('list')
    .alias('ls')
    .description('List all ACME orders')
    .option('--json', 'Output in JSON format')
    .action(listOrders)

  orders
    .command('show')
    .description('Show ACME order for a domain')
    .requiredOption('--domain-id <id>', 'Domain ID')
    .option('--json', 'Output in JSON format')
    .action(showOrder)

  orders
    .command('create')
    .description('Create or recreate an ACME order for a domain')
    .requiredOption('--domain-id <id>', 'Domain ID')
    .action(createOrder)

  orders
    .command('finalize')
    .description('Finalize an ACME order (complete challenge validation)')
    .requiredOption('--domain-id <id>', 'Domain ID')
    .action(finalizeOrder)

  orders
    .command('cancel')
    .description('Cancel an ACME order for a domain')
    .requiredOption('--domain-id <id>', 'Domain ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(cancelOrder)

  // --- Standalone domain subcommands ---
  domains
    .command('dns-challenge')
    .description('Setup DNS challenge records automatically using a DNS provider')
    .requiredOption('--domain-id <id>', 'Domain ID')
    .requiredOption('--provider-id <id>', 'DNS provider ID')
    .action(dnsChallengeSetup)

  domains
    .command('http-debug')
    .description('Debug HTTP-01 challenge for a domain')
    .requiredOption('-d, --domain <domain>', 'Domain name')
    .option('--json', 'Output in JSON format')
    .action(httpChallengeDebug)
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
    const baseDomain = wildcardBaseDomain(domain)
    newline()
    box(
      `Type: TXT\n` +
      `Name: _acme-challenge.${baseDomain}\n` +
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
    if (isProvisionedStatus(domainData.status)) {
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

async function listRenewalAttempts(options: RenewalAttemptsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const page = Number.parseInt(options.page ?? '1', 10) || 1
  const pageSize = Number.parseInt(options.pageSize ?? '20', 10) || 20

  const result = await withSpinner(`Fetching renewal attempts for ${options.domain}...`, async () => {
    const { data, error } = await listRenewalAttemptsApi({
      client,
      path: { domain: options.domain },
      query: { page, page_size: pageSize },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.lock} Renewal Attempts for ${options.domain} (${result?.total ?? 0} total, page ${result?.page ?? page}/${Math.max(1, Math.ceil((result?.total ?? 0) / (result?.page_size ?? pageSize)))})`)

  const attempts = result?.attempts ?? []
  if (attempts.length === 0) {
    info('No renewal attempts recorded for this domain')
    newline()
    return
  }

  const columns: TableColumn<RenewalAttemptResponse>[] = [
    {
      header: 'When',
      accessor: (a) => formatDate(new Date(a.created_at).toISOString()),
      color: (v) => colors.muted(v),
    },
    { header: 'Stage', key: 'stage' },
    { header: 'Method', key: 'verification_method' },
    { header: 'Outcome', key: 'outcome', color: (v) => statusBadge(v) },
    {
      header: 'Error',
      accessor: (a) => a.error ?? '-',
      color: (v) => (v === '-' ? colors.muted(v) : colors.error(v)),
    },
  ]

  printTable(attempts, columns)
  newline()
  if ((result?.total ?? 0) > page * pageSize) {
    info(`Run with --page ${page + 1} to see more`)
    newline()
  }
}

// --- ACME Orders ---

async function listOrders(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching ACME orders...', async () => {
    const { data, error } = await listOrdersApi({ client })
    if (error) throw new Error(getErrorMessage(error))
    return data?.orders ?? []
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.lock} ACME Orders (${result.length})`)

  if (result.length === 0) {
    info('No ACME orders found')
    info('Run: temps domains orders create --domain-id <id>')
    newline()
    return
  }

  const columns: TableColumn<AcmeOrderResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Domain ID', key: 'domain_id', width: 10 },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v) },
    { header: 'Email', key: 'email' },
    {
      header: 'Created',
      accessor: (o) => formatDate(new Date(o.created_at * 1000).toISOString()),
      color: (v) => colors.muted(v),
    },
    {
      header: 'Expires',
      accessor: (o) => o.expires_at ? formatDate(new Date(o.expires_at * 1000).toISOString()) : '-',
      color: (v) => colors.muted(v),
    },
  ]

  printTable(result, columns, { style: 'minimal' })
  newline()
}

async function showOrder(options: OrderShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const order = await withSpinner('Fetching ACME order...', async () => {
    const { data, error } = await getDomainOrder({
      client,
      path: { domain_id: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!order) {
    warning('No order found for this domain')
    return
  }

  if (options.json) {
    json(order)
    return
  }

  newline()
  header(`${icons.lock} ACME Order for Domain ${domainId}`)
  keyValue('Order ID', order.id)
  keyValue('Status', statusBadge(order.status))
  keyValue('Email', order.email)
  keyValue('Order URL', order.order_url)
  if (order.finalize_url) {
    keyValue('Finalize URL', order.finalize_url)
  }
  if (order.certificate_url) {
    keyValue('Certificate URL', order.certificate_url)
  }
  keyValue('Created', formatDate(new Date(order.created_at * 1000).toISOString()))
  keyValue('Updated', formatDate(new Date(order.updated_at * 1000).toISOString()))
  if (order.expires_at) {
    keyValue('Expires', formatDate(new Date(order.expires_at * 1000).toISOString()))
  }

  if (order.challenge_validation) {
    newline()
    header('Challenge Validation')
    keyValue('Type', order.challenge_validation.type)
    keyValue('Status', statusBadge(order.challenge_validation.status))
    keyValue('Token', order.challenge_validation.token)
    keyValue('URL', order.challenge_validation.url)
    if (order.challenge_validation.validated) {
      keyValue('Validated', order.challenge_validation.validated)
    }
    if (order.challenge_validation.error) {
      newline()
      warning(`Challenge Error: ${order.challenge_validation.error.detail ?? 'Unknown error'}`)
    }
  }

  if (order.error) {
    newline()
    warning(`Error: ${order.error}`)
    if (order.error_type) {
      keyValue('Error Type', order.error_type)
    }
  }

  newline()
}

async function createOrder(options: OrderCreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const result = await withSpinner(`Creating ACME order for domain ${domainId}...`, async () => {
    const { data, error } = await createOrRecreateOrder({
      client,
      path: { domain_id: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!result) {
    warning('No response received')
    return
  }

  newline()
  success(`ACME order created for ${result.domain}`)
  keyValue('Status', statusBadge(result.status))

  if (result.txt_records && result.txt_records.length > 0) {
    newline()
    header('DNS TXT Records to Add')
    for (const record of result.txt_records) {
      box(
        `Type: TXT\n` +
        `Name: ${record.name}\n` +
        `Value: ${record.value}`,
        'Add this DNS record'
      )
      newline()
    }
    info('After adding DNS records, run:')
    info(`  temps domains orders finalize --domain-id ${domainId}`)
  } else {
    newline()
    info('HTTP-01 challenge will be validated automatically')
    info(`Run: temps domains orders finalize --domain-id ${domainId}`)
  }
}

async function finalizeOrder(options: OrderFinalizeOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const result = await withSpinner(`Finalizing ACME order for domain ${domainId}...`, async () => {
    const { data, error } = await finalizeOrderApi({
      client,
      path: { domain_id: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!result) {
    warning('No response received')
    return
  }

  newline()
  if (isProvisionedStatus(result.status)) {
    success(`ACME order finalized for ${result.domain}`)
    keyValue('Status', statusBadge(result.status))
    if (result.expiration_time) {
      keyValue('Certificate Expires', formatDate(new Date(result.expiration_time * 1000).toISOString()))
    }
  } else {
    warning(`Order finalization returned status: ${result.status}`)
    keyValue('Domain', result.domain)
    keyValue('Status', statusBadge(result.status))
    if (result.last_error) {
      warning(`Error: ${result.last_error}`)
    }
  }
  newline()
}

async function cancelOrder(options: OrderCancelOptions): Promise<void> {
  await requireAuth()

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Cancel ACME order for domain ID ${domainId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await setupClient()

  const result = await withSpinner(`Cancelling ACME order for domain ${domainId}...`, async () => {
    const { data, error } = await cancelDomainOrder({
      client,
      path: { domain_id: domainId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  success(`ACME order cancelled for domain ${domainId}`)
  if (result) {
    keyValue('Domain', result.domain)
    keyValue('Status', statusBadge(result.status))
  }
  newline()
}

// --- DNS Challenge Setup ---

async function dnsChallengeSetup(options: DnsChallengeOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const providerId = parseInt(options.providerId, 10)
  if (isNaN(providerId)) {
    warning('Invalid DNS provider ID')
    return
  }

  const result = await withSpinner(`Setting up DNS challenge for domain ${domainId}...`, async () => {
    const { data, error } = await setupDnsChallengeApi({
      client,
      path: { domain_id: domainId },
      body: { dns_provider_id: providerId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!result) {
    warning('No response received')
    return
  }

  newline()
  if (result.success) {
    success(result.message)
    keyValue('Records Created', `${result.records_created}/${result.total_records}`)
  } else {
    warning(result.message)
    keyValue('Records Created', `${result.records_created}/${result.total_records}`)
  }

  if (result.results && result.results.length > 0) {
    newline()
    header('DNS Record Results')
    for (const record of result.results) {
      const statusIcon = record.success ? colors.success('OK') : colors.error('FAIL')
      console.log(`  ${statusIcon}  ${colors.bold(record.name)}`)
      console.log(`       Value: ${colors.muted(record.value)}`)
      console.log(`       ${record.message}`)
    }
  }

  if (result.success) {
    newline()
    info('DNS records created. You can now finalize the order:')
    info(`  temps domains orders finalize --domain-id ${domainId}`)
  }
  newline()
}

// --- HTTP Challenge Debug ---

async function httpChallengeDebug(options: HttpDebugOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const domainName = options.domain

  const result = await withSpinner(`Fetching HTTP challenge debug info for ${domainName}...`, async () => {
    const { data, error } = await getHttpChallengeDebug({
      client,
      path: { domain: domainName },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!result) {
    warning('No response received')
    return
  }

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.globe} HTTP Challenge Debug: ${domainName}`)
  keyValue('Domain', result.domain)
  keyValue('Challenge Exists', result.challenge_exists ? colors.success('Yes') : colors.error('No'))

  if (result.challenge_token) {
    keyValue('Challenge Token', result.challenge_token)
  }
  if (result.challenge_url) {
    keyValue('Challenge URL', result.challenge_url)
  }
  if (result.validation_url) {
    keyValue('Validation URL', result.validation_url)
  }

  newline()
  header('DNS Resolution')
  if (result.dns_a_records.length > 0) {
    keyValue('A Records', result.dns_a_records.join(', '))
  } else {
    keyValue('A Records', colors.muted('none'))
  }
  if (result.dns_aaaa_records.length > 0) {
    keyValue('AAAA Records', result.dns_aaaa_records.join(', '))
  } else {
    keyValue('AAAA Records', colors.muted('none'))
  }
  if (result.dns_error) {
    newline()
    warning(`DNS Error: ${result.dns_error}`)
  }

  if (!result.challenge_exists) {
    newline()
    info('No active HTTP challenge found for this domain.')
    info('Create an order first: temps domains orders create --domain-id <id>')
  }

  newline()
}
