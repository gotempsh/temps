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
import { promptText, promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, warning, info,
  keyValue, formatDate, box
} from '../../ui/output.js'

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
    .command('add <domain>')
    .description('Add a custom domain')
    .option('-c, --challenge <type>', 'Challenge type (http-01 or dns-01)', 'http-01')
    .action(addDomain)

  domains
    .command('verify <domain>')
    .description('Verify domain and provision SSL certificate')
    .action(verifyDomain)

  domains
    .command('remove <domain>')
    .alias('rm')
    .description('Remove a domain')
    .option('-f, --force', 'Skip confirmation')
    .action(removeDomain)

  domains
    .command('ssl <domain>')
    .description('Manage SSL certificate')
    .option('--renew', 'Force certificate renewal')
    .action(manageSsl)

  domains
    .command('status <domain>')
    .description('Check domain status')
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

async function addDomain(
  domain: string,
  options: { challenge: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()
  info(`Adding domain ${colors.bold(domain)}`)

  const result = await withSpinner('Adding domain...', async () => {
    const { data, error } = await createDomain({
      client,
      body: {
        domain,
        challenge_type: options.challenge,
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
    info('Run "temps domains verify ' + domain + '" after adding the record')
  } else if (options.challenge === 'http-01') {
    newline()
    info('HTTP-01 challenge will be validated automatically when provisioning')
    info('Run "temps domains verify ' + domain + '" to provision SSL certificate')
  }
}

async function verifyDomain(domain: string): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner(`Provisioning SSL for ${domain}...`, async () => {
    const { data, error } = await provisionDomain({
      client,
      path: { domain },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  if (result?.status === 'active' || result?.status === 'provisioned') {
    success(`Domain ${domain} verified and SSL certificate provisioned`)
  } else if (result?.status === 'pending') {
    info(`Domain ${domain} is pending verification`)
    info('Please ensure DNS records are properly configured')
  } else {
    warning(`Domain status: ${result?.status ?? 'unknown'}`)
    if (result?.last_error) {
      warning(`Error: ${result.last_error}`)
    }
  }
}

async function removeDomain(domain: string, options: { force?: boolean }): Promise<void> {
  await requireAuth()

  if (!options.force) {
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

async function manageSsl(domain: string, options: { renew?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  if (options.renew) {
    await withSpinner(`Renewing SSL certificate for ${domain}...`, async () => {
      const { error } = await renewDomain({
        client,
        path: { domain },
      })
      if (error) throw new Error(getErrorMessage(error))
    })
    success('SSL certificate renewal initiated')
    return
  }

  const sslInfo = await withSpinner('Fetching SSL info...', async () => {
    const { data, error } = await checkDomainStatus({
      client,
      path: { domain },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  header(`${icons.lock} SSL Certificate for ${domain}`)
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

async function domainStatus(domain: string): Promise<void> {
  await requireAuth()
  await setupClient()

  const status = await withSpinner(`Checking status for ${domain}...`, async () => {
    const { data, error } = await checkDomainStatus({
      client,
      path: { domain },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  header(`${icons.globe} Domain Status: ${domain}`)
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
