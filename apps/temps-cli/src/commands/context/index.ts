import type { Command } from 'commander'
import {
  listContexts,
  getActiveContext,
  setActiveContext,
  removeContext,
  contextsPath,
} from '../../config/contexts.js'
import { credentials } from '../../config/store.js'
import {
  success,
  info,
  warning,
  newline,
  header,
  icons,
  colors,
  json as jsonOutput,
  error as errorOutput,
} from '../../ui/output.js'
import { printTable, type TableColumn } from '../../ui/table.js'

interface ContextRow {
  name: string
  url: string
  email: string
  keyPrefix?: string
  expiresAt?: string
  isActive?: boolean
}

async function listAction(options: { json?: boolean }): Promise<void> {
  const contexts = await listContexts()

  if (contexts.length === 0) {
    if (options.json) {
      jsonOutput([])
      return
    }
    newline()
    info('No contexts configured.')
    info(`Run ${colors.bold('temps login <url>')} to create one.`)
    info(`Storage path: ${colors.muted(contextsPath())}`)
    newline()
    return
  }

  if (options.json) {
    jsonOutput(
      contexts.map((c) => ({
        name: c.name,
        url: c.url,
        email: c.email,
        keyPrefix: c.keyPrefix,
        expiresAt: c.expiresAt,
        isActive: !!c.isActive,
      })),
    )
    return
  }

  newline()
  header(`${icons.globe} CLI Contexts (${contexts.length})`)

  const columns: TableColumn<ContextRow>[] = [
    {
      header: '',
      accessor: (c) => (c.isActive ? colors.success('●') : colors.muted('○')),
    },
    { header: 'Name', key: 'name', color: (v) => colors.bold(String(v)) },
    { header: 'URL', key: 'url', color: (v) => colors.primary(String(v)) },
    {
      header: 'Email',
      accessor: (c) => c.email || colors.muted('-'),
    },
    {
      header: 'Key',
      accessor: (c) => (c.keyPrefix ? colors.muted(`${c.keyPrefix}…`) : colors.muted('-')),
    },
    {
      header: 'Expires',
      accessor: (c) =>
        c.expiresAt
          ? new Date(c.expiresAt).toISOString().split('T')[0] ?? '-'
          : colors.muted('-'),
    },
  ]

  printTable(contexts, columns, { style: 'minimal' })
  newline()
}

async function useAction(name: string): Promise<void> {
  const ok = await setActiveContext(name)
  if (!ok) {
    errorOutput(`Context "${name}" not found.`)
    const contexts = await listContexts()
    if (contexts.length > 0) {
      info(`Available: ${contexts.map((c) => c.name).join(', ')}`)
    }
    return
  }
  // The active context now drives `config.get('apiUrl')` and
  // `credentials.getApiKey()` — no need to mirror into the legacy stores.
  // But we DO want `temps whoami` and any code paths that still use the
  // legacy email field to reflect the new identity, so refresh it.
  const active = await getActiveContext()
  if (active) {
    await credentials.setAll({
      apiKey: active.apiKey,
      email: active.email,
    })
  }
  success(`Active context: ${colors.bold(name)}`)
  if (active) {
    info(`Server: ${colors.primary(active.url)}`)
    if (active.email) info(`User:   ${active.email}`)
  }
}

async function removeAction(name: string): Promise<void> {
  const removed = await removeContext(name)
  if (!removed) {
    errorOutput(`Context "${name}" not found.`)
    return
  }
  success(`Removed context "${name}"`)
  warning(
    'Server-side API key was NOT revoked. Run `temps logout --context <name>` first if you want to revoke it on the server.',
  )
  const remaining = await getActiveContext()
  if (remaining) {
    info(`Active context is now ${colors.bold(remaining.name)}.`)
  }
}

async function currentAction(options: { json?: boolean }): Promise<void> {
  const active = await getActiveContext()
  if (!active) {
    if (options.json) {
      jsonOutput(null)
      return
    }
    info('No active context. Run `temps login <url>` first.')
    return
  }
  if (options.json) {
    jsonOutput({
      name: active.name,
      url: active.url,
      email: active.email,
      keyPrefix: active.keyPrefix,
      expiresAt: active.expiresAt,
    })
    return
  }
  // Just print the name — friendly for shell scripting (`echo $(temps context current)`).
  console.log(active.name)
}

export function registerContextCommands(program: Command): void {
  const ctx = program
    .command('context')
    .description('Manage CLI contexts (one set of credentials per Temps server)')

  ctx
    .command('list')
    .alias('ls')
    .description('List all configured contexts')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  ctx
    .command('use <name>')
    .alias('switch')
    .description('Switch the active context')
    .action(useAction)

  ctx
    .command('remove <name>')
    .alias('rm')
    .description('Remove a context (does NOT revoke the key on the server)')
    .action(removeAction)

  ctx
    .command('current')
    .description('Print the active context name')
    .option('--json', 'Output in JSON format with full details')
    .action(currentAction)
}
