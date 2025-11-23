import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptSelect, promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, warning, info,
  keyValue, formatDate, box
} from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Domain {
  id: number
  domain: string
  project_name?: string
  environment?: string
  status: string
  ssl_status?: string
  created_at: string
  verified_at?: string
}

export function registerDomainsCommands(program: Command): void {
  const domains = program
    .command('domains')
    .alias('domain')
    .description('Manage custom domains')

  domains
    .command('list [project]')
    .alias('ls')
    .description('List domains')
    .option('--json', 'Output in JSON format')
    .action(listDomains)

  domains
    .command('add <domain>')
    .description('Add a custom domain')
    .option('-p, --project <project>', 'Project name')
    .option('-e, --environment <env>', 'Environment', 'production')
    .action(addDomain)

  domains
    .command('verify <domain>')
    .description('Verify domain ownership')
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
}

async function listDomains(project: string | undefined, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  const client = getClient()

  const domains = await withSpinner('Fetching domains...', async () => {
    const endpoint = project ? '/api/projects/{project}/domains' : '/api/domains'
    const response = await client.get(endpoint as '/api/domains', {
      params: project ? { path: { project } } : undefined,
    } as never)
    return (response.data ?? []) as Domain[]
  })

  if (options.json) {
    json(domains)
    return
  }

  newline()
  header(`${icons.globe} Domains (${domains.length})`)

  const columns: TableColumn<Domain>[] = [
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Project', accessor: (d) => d.project_name ?? '-' },
    { header: 'Environment', accessor: (d) => d.environment ?? 'production' },
    { header: 'Status', accessor: (d) => d.status, color: (v) => statusBadge(v) },
    { header: 'SSL', accessor: (d) => d.ssl_status ?? 'pending', color: (v) => statusBadge(v) },
  ]

  printTable(domains, columns, { style: 'minimal' })
  newline()
}

async function addDomain(
  domain: string,
  options: { project?: string; environment: string }
): Promise<void> {
  await requireAuth()
  const client = getClient()

  const projectName = options.project ?? await promptText({
    message: 'Project name',
    required: true,
  })

  newline()
  info(`Adding domain ${colors.bold(domain)} to ${colors.bold(projectName)}`)

  const result = await withSpinner('Adding domain...', async () => {
    const response = await client.post('/api/domains' as never, {
      body: {
        domain,
        project_name: projectName,
        environment: options.environment,
      },
    })
    return response.data as Domain & { verification_record?: { type: string; name: string; value: string } }
  })

  newline()
  success(`Domain ${domain} added`)

  if (result.verification_record) {
    newline()
    box(
      `Type: ${result.verification_record.type}\n` +
      `Name: ${result.verification_record.name}\n` +
      `Value: ${result.verification_record.value}`,
      'Add this DNS record to verify ownership'
    )
    newline()
    info('Run "temps domains verify ' + domain + '" after adding the record')
  }
}

async function verifyDomain(domain: string): Promise<void> {
  await requireAuth()
  const client = getClient()

  const result = await withSpinner(`Verifying ${domain}...`, async () => {
    const response = await client.post('/api/domains/{domain}/verify' as never, {
      params: { path: { domain } },
    })
    return response.data as { verified: boolean; message?: string }
  })

  newline()
  if (result.verified) {
    success(`Domain ${domain} verified successfully`)
    info('SSL certificate will be provisioned automatically')
  } else {
    warning(`Domain verification failed: ${result.message ?? 'Unknown error'}`)
    info('Please check your DNS records and try again')
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

  const client = getClient()

  await withSpinner(`Removing ${domain}...`, async () => {
    await client.delete('/api/domains/{domain}' as never, {
      params: { path: { domain } },
    })
  })

  success(`Domain ${domain} removed`)
}

async function manageSsl(domain: string, options: { renew?: boolean }): Promise<void> {
  await requireAuth()
  const client = getClient()

  if (options.renew) {
    await withSpinner(`Renewing SSL certificate for ${domain}...`, async () => {
      await client.post('/api/domains/{domain}/ssl/renew' as never, {
        params: { path: { domain } },
      })
    })
    success('SSL certificate renewal initiated')
    return
  }

  const sslInfo = await withSpinner('Fetching SSL info...', async () => {
    const response = await client.get('/api/domains/{domain}/ssl' as never, {
      params: { path: { domain } },
    })
    return response.data as {
      status: string
      issuer?: string
      expires_at?: string
      auto_renew: boolean
    }
  })

  newline()
  header(`${icons.lock} SSL Certificate for ${domain}`)
  keyValue('Status', statusBadge(sslInfo.status))
  keyValue('Issuer', sslInfo.issuer)
  keyValue('Expires', sslInfo.expires_at ? formatDate(sslInfo.expires_at) : '-')
  keyValue('Auto-renew', sslInfo.auto_renew ? 'Enabled' : 'Disabled')
  newline()
}
