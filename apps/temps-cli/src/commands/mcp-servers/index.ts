// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { readFileSync } from 'node:fs'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
} from '../../ui/output.js'

// --- Types ---

interface McpDefinition {
  id: number
  slug: string
  name: string
  description?: string | null
  config: Record<string, unknown>
  project_id?: number | null
}

interface ListResponse {
  items: McpDefinition[]
  total: number
}

// --- Helpers ---

/** Resolve value: if prefixed with @, read from file path */
export function resolveValue(value: string): string {
  if (value.startsWith('@')) {
    const filePath = value.slice(1)
    try {
      return readFileSync(filePath, 'utf-8')
    } catch (e) {
      throw new Error(`Failed to read file '${filePath}': ${e}`)
    }
  }
  return value
}

export function parseJson(value: string): Record<string, unknown> {
  const raw = resolveValue(value)
  try {
    return JSON.parse(raw)
  } catch (e) {
    throw new Error(`Invalid JSON: ${e}`)
  }
}

async function resolveProjectId(projectSlug: string): Promise<number> {
  const { data, error } = await client.get({
    url: '/projects/by-slug/{slug}',
    path: { slug: projectSlug },
  })
  if (!error && data) {
    return (data as { id: number }).id
  }

  const parsed = parseInt(projectSlug, 10)
  if (!isNaN(parsed)) return parsed

  throw new Error(`Project '${projectSlug}' not found`)
}

// --- Options ---

interface ListOptions {
  global?: boolean
  project?: string
  json?: boolean
}

interface CreateOptions {
  name: string
  slug: string
  config: string
  description?: string
  global?: boolean
  project?: string
}

interface UpdateOptions {
  name?: string
  config?: string
  description?: string
  global?: boolean
  project?: string
}

interface DeleteOptions {
  global?: boolean
  project?: string
  force?: boolean
  yes?: boolean
}

// --- Registration ---

export function registerMcpServersCommands(program: Command): void {
  const mcp = program
    .command('mcp-servers')
    .description('Manage MCP server definitions (global or project-scoped)')

  mcp
    .command('list')
    .alias('ls')
    .description('List MCP server definitions')
    .option('--global', 'List global (platform-wide) MCP servers')
    .option('--project <slug>', 'List MCP servers for a specific project')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  mcp
    .command('create')
    .alias('add')
    .description('Create a new MCP server definition')
    .requiredOption('-n, --name <name>', 'MCP server name')
    .requiredOption('-s, --slug <slug>', 'MCP server slug (URL-safe identifier)')
    .requiredOption(
      '-c, --config <config>',
      'MCP server config (JSON). Prefix with @ to read from file (e.g. @./mcp.json)',
    )
    .option('-d, --description <description>', 'MCP server description')
    .option('--global', 'Create as global (platform-wide) MCP server')
    .option('--project <slug>', 'Create MCP server for a specific project')
    .action(createAction)

  mcp
    .command('update')
    .description('Update an existing MCP server definition')
    .argument('<slug>', 'Slug of the MCP server to update')
    .option('-n, --name <name>', 'New name')
    .option(
      '-c, --config <config>',
      'New config (JSON). Prefix with @ to read from file',
    )
    .option('-d, --description <description>', 'New description')
    .option('--global', 'Update a global MCP server')
    .option('--project <slug>', 'Update a project-scoped MCP server')
    .action(updateAction)

  mcp
    .command('delete')
    .alias('rm')
    .description('Delete an MCP server definition')
    .argument('<slug>', 'Slug of the MCP server to delete')
    .option('--global', 'Delete a global MCP server')
    .option('--project <slug>', 'Delete a project-scoped MCP server')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(deleteAction)
}

// --- Actions ---

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const isProject = !!options.project

  const items = await withSpinner('Fetching MCP servers...', async () => {
    let url: string
    let pathParams: Record<string, unknown> = {}

    if (isProject) {
      const pid = await resolveProjectId(options.project!)
      url = '/projects/{project_id}/mcp-servers'
      pathParams = { project_id: pid }
    } else {
      url = '/settings/mcp-servers'
    }

    const { data, error } = await client.get({ url, path: pathParams })
    if (error) throw new Error(getErrorMessage(error))
    return data as ListResponse
  })

  if (options.json) {
    json(items)
    return
  }

  const scopeLabel = isProject ? `Project (${options.project})` : 'Global'
  newline()
  header(`${icons.info} ${scopeLabel} MCP Servers (${items.items.length})`)

  if (items.items.length === 0) {
    info('No MCP servers defined yet.')
    info(
      isProject
        ? `Run: temps mcp-servers create --project ${options.project} --name "My Server" --slug my-server --config @./mcp.json`
        : 'Run: temps mcp-servers create --global --name "My Server" --slug my-server --config @./mcp.json',
    )
    newline()
    return
  }

  for (const mcp of items.items) {
    const scopeBadge = mcp.project_id
      ? colors.info('project')
      : colors.warning('global')
    console.log(
      `  ${colors.primary(mcp.slug)} ${colors.bold(mcp.name)} [${scopeBadge}]`,
    )
    if (mcp.description) {
      console.log(`    ${colors.muted(mcp.description)}`)
    }
    const cmd = mcp.config.command as string | undefined
    const args = mcp.config.args as string[] | undefined
    if (cmd) {
      const argsStr = args ? ` ${args.join(' ')}` : ''
      console.log(
        `    ${colors.muted('Command:')} ${colors.bold(cmd)}${colors.muted(argsStr)}`,
      )
    }
    newline()
  }
}

async function createAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const config = parseJson(options.config)
  const isProject = !!options.project

  const mcp = await withSpinner('Creating MCP server...', async () => {
    let url: string
    let pathParams: Record<string, unknown> = {}

    if (isProject) {
      const pid = await resolveProjectId(options.project!)
      url = '/projects/{project_id}/mcp-servers'
      pathParams = { project_id: pid }
    } else {
      url = '/settings/mcp-servers'
    }

    const { data, error } = await client.post({
      url,
      path: pathParams,
      body: {
        slug: options.slug,
        name: options.name,
        description: options.description || undefined,
        config,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as McpDefinition
  })

  success(`MCP server created: ${mcp.name} (${mcp.slug})`)
}

async function updateAction(
  slug: string,
  options: UpdateOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const config = options.config ? parseJson(options.config) : undefined
  const isProject = !!options.project

  const body: Record<string, unknown> = {}
  if (options.name) body.name = options.name
  if (options.description !== undefined) body.description = options.description
  if (config) body.config = config

  if (Object.keys(body).length === 0) {
    warning(
      'No fields to update. Provide at least one of --name, --config, or --description',
    )
    return
  }

  const mcp = await withSpinner('Updating MCP server...', async () => {
    let url: string
    let pathParams: Record<string, unknown> = {}

    if (isProject) {
      const pid = await resolveProjectId(options.project!)
      url = '/projects/{project_id}/mcp-servers/{slug}'
      pathParams = { project_id: pid, slug }
    } else {
      url = '/settings/mcp-servers/{slug}'
      pathParams = { slug }
    }

    const { data, error } = await client.put({ url, path: pathParams, body })
    if (error) throw new Error(getErrorMessage(error))
    return data as McpDefinition
  })

  success(`MCP server updated: ${mcp.name} (${mcp.slug})`)
}

async function deleteAction(
  slug: string,
  options: DeleteOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete MCP server "${slug}"? This cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const isProject = !!options.project

  await withSpinner('Deleting MCP server...', async () => {
    let url: string
    let pathParams: Record<string, unknown> = {}

    if (isProject) {
      const pid = await resolveProjectId(options.project!)
      url = '/projects/{project_id}/mcp-servers/{slug}'
      pathParams = { project_id: pid, slug }
    } else {
      url = '/settings/mcp-servers/{slug}'
      pathParams = { slug }
    }

    const { error } = await client.delete({ url, path: pathParams })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`MCP server "${slug}" deleted`)
}
