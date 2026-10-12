// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listStatusPages,
  createStatusPage,
  getStatusPage,
  updateStatusPage,
  publishStatusPage,
  unpublishStatusPage,
  rotateStatusPageUrl,
  deleteStatusPage,
  listStatusPageDomains,
  addStatusPageDomain,
  verifyStatusPageDomain,
  renewStatusPageDomain,
  removeStatusPageDomain,
} from '../../api/sdk.gen.js'
import type {
  StatusPageDomainResponse,
  StatusPageMonitorInput,
  StatusPageResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning,
  keyValue, formatDate,
} from '../../ui/output.js'

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  title: string
  description?: string
  monitor?: string[]
  json?: boolean
}

interface IdOptions {
  id: string
  json?: boolean
  yes?: boolean
}

interface UpdateOptions extends IdOptions {
  title?: string
  description?: string
  monitor?: string[]
  clearMonitors?: boolean
}

function collect(value: string, previous: string[] = []): string[] {
  return [...previous, value]
}

/**
 * Parse `--monitor <id>=<public name>` into a selection entry, or return an
 * error message. The public name is what visitors see; it is required so an
 * internal monitor name is never published by default.
 */
export function parseMonitorSpec(spec: string): StatusPageMonitorInput | string {
  const separator = spec.indexOf('=')
  if (separator <= 0) {
    return `Invalid --monitor "${spec}": use <monitor-id>=<public name>, e.g. 12=API`
  }
  const id = Number(spec.slice(0, separator).trim())
  const name = spec.slice(separator + 1).trim()
  if (!Number.isInteger(id) || id <= 0) {
    return `Invalid monitor id in --monitor "${spec}"`
  }
  if (!name) {
    return `Missing public name in --monitor "${spec}"`
  }
  return { monitor_id: id, display_name: name }
}

export function parseMonitorSpecs(
  specs: string[] | undefined,
): StatusPageMonitorInput[] | string {
  const selection: StatusPageMonitorInput[] = []
  for (const spec of specs ?? []) {
    const parsed = parseMonitorSpec(spec)
    if (typeof parsed === 'string') return parsed
    selection.push(parsed)
  }
  return selection
}

function parseId(value: string, what: string): number | null {
  const id = parseInt(value, 10)
  if (isNaN(id)) {
    warning(`Invalid ${what}`)
    return null
  }
  return id
}

function printPage(page: StatusPageResponse): void {
  newline()
  header(`${icons.info} ${page.title}`)
  keyValue('ID', page.id)
  keyValue('State', page.is_published ? colors.success('published') : colors.muted('draft'))
  keyValue('Public URL', page.public_url)
  keyValue('JSON', page.public_json_url)
  if (!page.public_url_configured) {
    warning(
      'No public URL is configured, so these links use the console address and only work '
      + `where the console is reachable. Set the external URL in the console (${page.public_url_setup_path ?? '/settings'}).`,
    )
  }
  if (page.published_at) {
    keyValue('Published', formatDate(page.published_at))
  }
  if (page.description) {
    keyValue('Description', page.description)
  }
  newline()
  if (page.monitors.length === 0) {
    info('No monitors selected. Add some with: temps status-pages update --id '
      + `${page.id} --monitor <monitor-id>=<public name>`)
  } else {
    printTable(page.monitors, [
      { header: 'Monitor ID', key: 'monitor_id', width: 10 },
      { header: 'Internal name (private)', key: 'monitor_name', color: (v) => colors.muted(v) },
      { header: 'Public name', key: 'display_name', color: (v) => colors.bold(v) },
    ], { style: 'minimal' })
  }
  if (!page.is_published) {
    info(`The public URL returns 404 until you run: temps status-pages publish --id ${page.id}`)
  }
  newline()
}

async function run<T>(label: string, call: () => Promise<{ data?: T; error?: unknown }>): Promise<T> {
  return withSpinner(label, async () => {
    const { data, error } = await call()
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (data === undefined) {
      throw new Error('The server returned an empty response')
    }
    return data
  })
}

interface DomainOptions {
  id: string
  domainId?: string
  hostname?: string
  dns?: boolean
  json?: boolean
  yes?: boolean
}

export function domainStateLabel(domain: Pick<StatusPageDomainResponse, 'status' | 'serving'>): string {
  switch (domain.status) {
    case 'active':
      return domain.serving ? 'serving' : 'verified (page unpublished)'
    case 'failed':
      return 'verification failed'
    case 'unverified':
      return 'needs verification'
    case 'renewal_pending':
      return 'serving, renewal in progress'
    case 'renewal_failed':
      return 'serving, renewal failed'
    default:
      return 'waiting for DNS'
  }
}

/** The next command to run for a domain, or null when nothing is needed. */
export function domainNextStep(
  domain: Pick<StatusPageDomainResponse, 'status' | 'verification_method'>,
): string | null {
  switch (domain.status) {
    case 'active':
      return domain.verification_method === 'dns-01'
        ? 'DNS-verified certificates are not renewed automatically. Before expiry run: temps status-pages domains renew'
        : null
    case 'renewal_pending':
      return domain.verification_method === 'dns-01'
        ? 'Publish the new TXT record below (the current certificate keeps serving), then run: temps status-pages domains verify'
        : 'Finish the renewal with: temps status-pages domains verify'
    case 'renewal_failed':
      return 'The current certificate keeps serving until it expires. Retry with: temps status-pages domains renew'
    default:
      return 'Create these DNS records, then run: temps status-pages domains verify'
  }
}

function printDomain(domain: StatusPageDomainResponse): void {
  newline()
  header(`${icons.info} ${domain.hostname}`)
  keyValue('Domain ID', domain.id)
  keyValue('State', domainStateLabel(domain))
  keyValue('Verification', domain.verification_method === 'dns-01' ? 'DNS TXT record' : 'HTTP (hostname points here)')
  if (domain.certificate_expires_at) {
    keyValue('Certificate expires', formatDate(domain.certificate_expires_at))
  }
  if (domain.last_error) {
    keyValue('Last error', domain.last_error)
  }
  const next = domainNextStep(domain)
  if (next) {
    newline()
    info(next)
  }
  if (domain.status !== 'active') {
    printTable(domain.dns_records, [
      { header: 'Type', key: 'record_type', width: 6 },
      { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
      { header: 'Value', key: 'value' },
    ], { style: 'minimal' })
  }
  newline()
}

function registerDomainCommands(pages: Command): void {
  const domains = pages
    .command('domains')
    .alias('domain')
    .description('Serve a status page on your own hostname')

  domains
    .command('list')
    .alias('ls')
    .description('List a status page\'s custom domains and the DNS records they need')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: DomainOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const result = await run('Fetching custom domains...', () =>
        listStatusPageDomains({ client, path: { status_page_id: id } }))
      if (options.json) {
        json(result)
        return
      }
      if (result.length === 0) {
        newline()
        info(`No custom domains. Add one with: temps status-pages domains add --id ${id} --hostname status.example.com`)
        newline()
        return
      }
      result.forEach(printDomain)
    })

  domains
    .command('add')
    .description('Attach a hostname; it serves only after ownership is verified')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--hostname <hostname>', 'Hostname, e.g. status.example.com')
    .option('--dns', 'Verify with a DNS TXT record instead of HTTP')
    .option('--json', 'Output in JSON format')
    .action(async (options: DomainOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const domain = await run('Adding custom domain...', () =>
        addStatusPageDomain({
          client,
          path: { status_page_id: id },
          body: {
            hostname: options.hostname ?? '',
            verification_method: options.dns ? 'dns-01' : 'http-01',
          },
        }))
      if (options.json) {
        json(domain)
        return
      }
      success(`Added ${domain.hostname}`)
      printDomain(domain)
    })

  domains
    .command('verify')
    .description('Prove ownership and issue the certificate')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID (see `domains list`)')
    .option('--json', 'Output in JSON format')
    .action(async (options: DomainOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const domainId = parseId(options.domainId ?? '', 'domain ID')
      if (id === null || domainId === null) return
      const domain = await run('Verifying...', () =>
        verifyStatusPageDomain({ client, path: { status_page_id: id, domain_id: domainId } }))
      if (options.json) {
        json(domain)
        return
      }
      if (domain.status === 'active') {
        success(`${domain.hostname} is verified`)
      } else {
        warning(`${domain.hostname} is not verified yet`)
      }
      printDomain(domain)
    })

  domains
    .command('renew')
    .description('Renew the certificate; the current one keeps serving meanwhile (DNS-01: prints the new TXT record)')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID (see `domains list`)')
    .option('--json', 'Output in JSON format')
    .action(async (options: DomainOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const domainId = parseId(options.domainId ?? '', 'domain ID')
      if (id === null || domainId === null) return
      const domain = await run('Renewing...', () =>
        renewStatusPageDomain({ client, path: { status_page_id: id, domain_id: domainId } }))
      if (options.json) {
        json(domain)
        return
      }
      if (domain.status === 'active') {
        success(`Renewed the certificate of ${domain.hostname}`)
      } else {
        info(`Renewal of ${domain.hostname} started`)
      }
      printDomain(domain)
    })

  domains
    .command('remove')
    .alias('rm')
    .description('Detach a hostname; it answers 404 immediately and its certificate is deleted')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID (see `domains list`)')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .action(async (options: DomainOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const domainId = parseId(options.domainId ?? '', 'domain ID')
      if (id === null || domainId === null) return
      if (!options.yes) {
        const confirmed = await promptConfirm({
          message: `Remove custom domain ${domainId}? It stops serving the page immediately.`,
          default: false,
        })
        if (!confirmed) {
          info('Cancelled')
          return
        }
      }
      await withSpinner('Removing custom domain...', async () => {
        const { error } = await removeStatusPageDomain({
          client,
          path: { status_page_id: id, domain_id: domainId },
        })
        if (error) {
          throw new Error(getErrorMessage(error))
        }
      })
      success(`Removed custom domain ${domainId}`)
    })
}

export function registerStatusPagesCommands(program: Command): void {
  const pages = program
    .command('status-pages')
    .alias('status-page')
    .description('Publish public status pages for selected monitors')

  pages
    .command('list')
    .alias('ls')
    .description('List a project\'s status pages')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: ListOptions) => {
      await requireAuth()
      await setupClient()
      const projectId = parseId(options.projectId, 'project ID')
      if (projectId === null) return
      const result = await run('Fetching status pages...', () =>
        listStatusPages({ client, path: { project_id: projectId }, query: { page_size: 100 } }))
      if (options.json) {
        json(result)
        return
      }
      newline()
      header(`${icons.info} Status pages for project ${projectId} (${result.total})`)
      if (result.items.length === 0) {
        info(`Create one with: temps status-pages create --project-id ${projectId} --title "Status" --monitor <monitor-id>=API`)
        newline()
        return
      }
      const columns: TableColumn<StatusPageResponse>[] = [
        { header: 'ID', key: 'id', width: 6 },
        { header: 'Title', key: 'title', color: (v) => colors.bold(v) },
        { header: 'State', accessor: (p) => (p.is_published ? 'published' : 'draft') },
        { header: 'Monitors', accessor: (p) => String(p.monitors.length) },
        { header: 'Public URL', accessor: (p) => (p.is_published ? p.public_url : '-') },
      ]
      printTable(result.items, columns, { style: 'minimal' })
      newline()
    })

  pages
    .command('create')
    .description('Create a draft status page')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('-t, --title <title>', 'Public page title')
    .option('-d, --description <text>', 'Public description')
    .option('-m, --monitor <id=name>', 'Monitor to show and its public name (repeatable)', collect)
    .option('--json', 'Output in JSON format')
    .action(async (options: CreateOptions) => {
      await requireAuth()
      await setupClient()
      const projectId = parseId(options.projectId, 'project ID')
      if (projectId === null) return
      const monitors = parseMonitorSpecs(options.monitor)
      if (typeof monitors === 'string') {
        warning(monitors)
        return
      }
      const page = await run('Creating status page...', () =>
        createStatusPage({
          client,
          path: { project_id: projectId },
          body: { title: options.title, description: options.description ?? null, monitors },
        }))
      if (options.json) {
        json(page)
        return
      }
      success(`Created draft status page ${page.id}`)
      printPage(page)
    })

  pages
    .command('show')
    .description('Show a status page')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: IdOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const page = await run('Fetching status page...', () =>
        getStatusPage({ client, path: { status_page_id: id } }))
      if (options.json) {
        json(page)
        return
      }
      printPage(page)
    })

  pages
    .command('update')
    .description('Change the title, description or monitor selection')
    .requiredOption('--id <id>', 'Status page ID')
    .option('-t, --title <title>', 'Public page title')
    .option('-d, --description <text>', 'Public description ("" clears it)')
    .option('-m, --monitor <id=name>', 'Replace the selection; repeat for each monitor', collect)
    .option('--clear-monitors', 'Remove every monitor (drafts only)')
    .option('--json', 'Output in JSON format')
    .action(async (options: UpdateOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      let monitors: StatusPageMonitorInput[] | undefined
      if (options.clearMonitors) {
        monitors = []
      } else if (options.monitor) {
        const parsed = parseMonitorSpecs(options.monitor)
        if (typeof parsed === 'string') {
          warning(parsed)
          return
        }
        monitors = parsed
      }
      const page = await run('Updating status page...', () =>
        updateStatusPage({
          client,
          path: { status_page_id: id },
          body: {
            ...(options.title !== undefined && { title: options.title }),
            ...(options.description !== undefined && { description: options.description }),
            ...(monitors !== undefined && { monitors }),
          },
        }))
      if (options.json) {
        json(page)
        return
      }
      success('Status page updated')
      printPage(page)
    })

  for (const [name, description, label, call, done] of [
    ['publish', 'Publish the page at its public URL', 'Publishing...', publishStatusPage, 'Published'],
    ['unpublish', 'Unpublish; the public URL returns 404 immediately', 'Unpublishing...', unpublishStatusPage, 'Unpublished'],
    ['rotate-url', 'Replace the public URL; the old one stops working', 'Replacing URL...', rotateStatusPageUrl, 'Public URL replaced'],
  ] as const) {
    pages
      .command(name)
      .description(description)
      .requiredOption('--id <id>', 'Status page ID')
      .option('--json', 'Output in JSON format')
      .action(async (options: IdOptions) => {
        await requireAuth()
        await setupClient()
        const id = parseId(options.id, 'status page ID')
        if (id === null) return
        const page = await run(label, () => call({ client, path: { status_page_id: id } }))
        if (options.json) {
          json(page)
          return
        }
        success(done)
        printPage(page)
      })
  }

  pages
    .command('delete')
    .alias('rm')
    .description('Delete a status page; its public URL returns 404 immediately')
    .requiredOption('--id <id>', 'Status page ID')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .action(async (options: IdOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      if (!options.yes) {
        const confirmed = await promptConfirm({
          message: `Delete status page ${id}? Its public URL stops working.`,
          default: false,
        })
        if (!confirmed) {
          info('Cancelled')
          return
        }
      }
      await withSpinner('Deleting status page...', async () => {
        const { error } = await deleteStatusPage({ client, path: { status_page_id: id } })
        if (error) {
          throw new Error(getErrorMessage(error))
        }
      })
      success(`Deleted status page ${id}`)
    })

  registerDomainCommands(pages)
}
