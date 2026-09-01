// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listCustomDomainsForProject,
  createCustomDomain,
  getCustomDomain,
  updateCustomDomain,
  deleteCustomDomain,
  linkCustomDomainToCertificate,
} from '../../api/sdk.gen.js'
import type { CustomDomainResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  domain?: string
  environmentId?: string
  branch?: string
  redirectTo?: string
  statusCode?: string
  yes?: boolean
}

interface ShowOptions {
  projectId: string
  domainId: string
  json?: boolean
}

interface UpdateOptions {
  projectId: string
  domainId: string
  domain?: string
  environmentId?: string
  branch?: string
  redirectTo?: string
  statusCode?: string
}

interface RemoveOptions {
  projectId: string
  domainId: string
  force?: boolean
  yes?: boolean
}

interface LinkCertOptions {
  projectId: string
  domainId: string
  certificateId: string
}

export function registerCustomDomainsCommands(program: Command): void {
  const customDomains = program
    .command('custom-domains')
    .alias('cdom')
    .description('Manage project custom domains')

  customDomains
    .command('list')
    .alias('ls')
    .description('List custom domains for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listCustomDomains)

  customDomains
    .command('create')
    .alias('add')
    .description('Create a custom domain for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-d, --domain <domain>', 'Domain name')
    .option('--environment-id <id>', 'Environment ID', '0')
    .option('--branch <branch>', 'Branch name')
    .option('--redirect-to <url>', 'Redirect target URL')
    .option('--status-code <code>', 'HTTP status code for redirects')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createCustomDomainAction)

  customDomains
    .command('show')
    .description('Show custom domain details')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID')
    .option('--json', 'Output in JSON format')
    .action(showCustomDomain)

  customDomains
    .command('update')
    .description('Update a custom domain')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID')
    .option('-d, --domain <domain>', 'New domain name')
    .option('--environment-id <id>', 'New environment ID')
    .option('--branch <branch>', 'New branch name')
    .option('--redirect-to <url>', 'New redirect target URL')
    .option('--status-code <code>', 'New HTTP status code for redirects')
    .action(updateCustomDomainAction)

  customDomains
    .command('remove')
    .alias('rm')
    .description('Remove a custom domain')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(removeCustomDomain)

  customDomains
    .command('link-cert')
    .description('Link a custom domain to a certificate')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--domain-id <id>', 'Custom domain ID')
    .requiredOption('--certificate-id <id>', 'Certificate ID')
    .action(linkCertificate)
}

async function listCustomDomains(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const domainsData = await withSpinner('Fetching custom domains...', async () => {
    const { data, error } = await listCustomDomainsForProject({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data?.domains ?? []
  })

  if (options.json) {
    json(domainsData)
    return
  }

  newline()
  header(`${icons.globe} Custom Domains for Project ${projectId} (${domainsData.length})`)

  if (domainsData.length === 0) {
    info('No custom domains configured')
    info(`Run: temps custom-domains create --project-id ${projectId} --domain example.com -y`)
    newline()
    return
  }

  const columns: TableColumn<CustomDomainResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Domain', key: 'domain', color: (v) => colors.bold(v) },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v) },
    { header: 'Environment', accessor: (d) => d.environment?.name ?? '-' },
    { header: 'Branch', accessor: (d) => d.branch ?? '-' },
    { header: 'Redirect', accessor: (d) => d.redirect_to ?? '-', color: (v) => colors.muted(v) },
    { header: 'Created', accessor: (d) => new Date(d.created_at * 1000).toLocaleDateString(), color: (v) => colors.muted(v) },
  ]

  printTable(domainsData, columns, { style: 'minimal' })
  newline()
}

async function createCustomDomainAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let domain: string
  let environmentId: number

  if (options.yes && options.domain) {
    domain = options.domain
    environmentId = options.environmentId ? parseInt(options.environmentId, 10) : 0
  } else {
    domain = options.domain || await promptText({
      message: 'Domain name',
      required: true,
    })

    const envInput = options.environmentId || await promptText({
      message: 'Environment ID (0 for production)',
      default: '0',
    })
    environmentId = parseInt(envInput, 10)
  }

  if (isNaN(environmentId)) {
    warning('Invalid environment ID')
    return
  }

  newline()
  info(`Adding custom domain ${colors.bold(domain)} to project ${projectId}`)

  const result = await withSpinner('Creating custom domain...', async () => {
    const { data, error } = await createCustomDomain({
      client,
      path: { project_id: projectId },
      body: {
        domain,
        environment_id: environmentId,
        ...(options.branch && { branch: options.branch }),
        ...(options.redirectTo && { redirect_to: options.redirectTo }),
        ...(options.statusCode && { status_code: parseInt(options.statusCode, 10) }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  newline()
  success(`Custom domain ${colors.bold(domain)} created`)
  if (result?.id) {
    info(`Domain ID: ${result.id}`)
    info(`Status: ${result.status}`)
  }
}

async function showCustomDomain(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const domain = await withSpinner('Fetching custom domain...', async () => {
    const { data, error } = await getCustomDomain({
      client,
      path: { project_id: projectId, domain_id: domainId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Custom domain ${options.domainId} not found`)
    }
    return data
  })

  if (options.json) {
    json(domain)
    return
  }

  newline()
  header(`${icons.globe} ${domain.domain}`)
  keyValue('ID', domain.id)
  keyValue('Domain', domain.domain)
  keyValue('Status', statusBadge(domain.status))
  keyValue('Project ID', domain.project_id)
  if (domain.environment) {
    keyValue('Environment', `${domain.environment.name} (${domain.environment.slug})`)
  }
  if (domain.branch) {
    keyValue('Branch', domain.branch)
  }
  if (domain.redirect_to) {
    keyValue('Redirect To', domain.redirect_to)
  }
  if (domain.status_code) {
    keyValue('Status Code', domain.status_code)
  }
  if (domain.domain_id) {
    keyValue('Linked Domain ID', domain.domain_id)
  }
  if (domain.expiration_time) {
    keyValue('Certificate Expires', new Date(domain.expiration_time * 1000).toLocaleString())
  }
  if (domain.last_renewed) {
    keyValue('Last Renewed', new Date(domain.last_renewed * 1000).toLocaleString())
  }
  if (domain.message) {
    keyValue('Message', domain.message)
  }
  keyValue('Created', new Date(domain.created_at * 1000).toLocaleString())
  keyValue('Updated', new Date(domain.updated_at * 1000).toLocaleString())
  newline()
}

// Only includes a field when the caller actually passed the flag, so an
// omitted option never overwrites existing state via a sparse PATCH. An
// explicitly empty `--branch ''`/`--redirect-to ''` clears the field (sent as
// `null`) rather than being dropped, which is the only way to unset it.
export function buildCustomDomainUpdateBody(options: {
  domain?: string
  environmentId?: string
  branch?: string
  redirectTo?: string
  statusCode?: string
}): Record<string, unknown> {
  const body: Record<string, unknown> = {}
  if (options.domain) {
    body.domain = options.domain
  }
  if (options.environmentId) {
    body.environment_id = parseInt(options.environmentId, 10)
  }
  if (options.branch !== undefined) {
    body.branch = options.branch || null
  }
  if (options.redirectTo !== undefined) {
    body.redirect_to = options.redirectTo || null
  }
  if (options.statusCode) {
    body.status_code = parseInt(options.statusCode, 10)
  }
  return body
}

async function updateCustomDomainAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const body = buildCustomDomainUpdateBody(options)

  if (Object.keys(body).length === 0) {
    warning('No update options provided')
    info('Use --domain, --environment-id, --branch, --redirect-to, or --status-code to update fields')
    return
  }

  const result = await withSpinner('Updating custom domain...', async () => {
    const { data, error } = await updateCustomDomain({
      client,
      path: { project_id: projectId, domain_id: domainId },
      body: body as never,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Custom domain #${domainId} updated`)
  if (result?.domain) {
    info(`Domain: ${result.domain}`)
  }
}

async function removeCustomDomain(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  // Get domain details first for confirmation message
  const { data: domain, error: getError } = await getCustomDomain({
    client,
    path: { project_id: projectId, domain_id: domainId },
  })

  if (getError || !domain) {
    warning(`Custom domain ${options.domainId} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove custom domain "${domain.domain}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing custom domain...', async () => {
    const { error } = await deleteCustomDomain({
      client,
      path: { project_id: projectId, domain_id: domainId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Custom domain "${domain.domain}" removed`)
}

async function linkCertificate(options: LinkCertOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const domainId = parseInt(options.domainId, 10)
  if (isNaN(domainId)) {
    warning('Invalid domain ID')
    return
  }

  const certificateId = parseInt(options.certificateId, 10)
  if (isNaN(certificateId)) {
    warning('Invalid certificate ID')
    return
  }

  const result = await withSpinner('Linking certificate...', async () => {
    const { data, error } = await linkCustomDomainToCertificate({
      client,
      path: { project_id: projectId, domain_id: domainId, certificate_id: certificateId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Certificate #${certificateId} linked to custom domain #${domainId}`)
  if (result?.domain) {
    info(`Domain: ${result.domain}`)
    info(`Status: ${result.status}`)
  }
}
