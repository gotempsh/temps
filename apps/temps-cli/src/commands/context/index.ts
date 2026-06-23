import type { Command } from 'commander'
import {
  listContexts,
  getActiveContext,
  setActiveContext,
  removeContext,
  renameContext,
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

  // `TEMPS_CONTEXT` overrides which context is active for the whole CLI, so
  // the active marker (and JSON `isActive`) must reflect the env selection,
  // not just the on-disk flag — otherwise `context ls` lies about what the
  // next command will actually use.
  const envContext = process.env.TEMPS_CONTEXT?.trim() || null
  const envContextExists = envContext
    ? contexts.some((c) => c.name === envContext)
    : false
  const isActiveRow = (c: ContextRow): boolean =>
    envContext ? c.name === envContext : !!c.isActive

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
        isActive: isActiveRow(c),
      })),
    )
    return
  }

  newline()
  header(`${icons.globe} CLI Contexts (${contexts.length})`)

  if (envContext) {
    if (envContextExists) {
      info(`Active context pinned by ${colors.bold('TEMPS_CONTEXT')}=${colors.primary(envContext)}`)
    } else {
      warning(
        `${colors.bold('TEMPS_CONTEXT')}=${envContext} does not match any context below — the CLI will be unauthenticated.`,
      )
    }
  }

  const columns: TableColumn<ContextRow>[] = [
    {
      header: '',
      accessor: (c) => (isActiveRow(c) ? colors.success('●') : colors.muted('○')),
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
  // `setActiveContext` updated the on-disk flag, but a live `TEMPS_CONTEXT`
  // env var still wins at resolution time. Warn so the user isn't surprised
  // that their switch appears to have no effect.
  const envContext = process.env.TEMPS_CONTEXT?.trim() || null
  if (envContext && envContext !== name) {
    warning(
      `${colors.bold('TEMPS_CONTEXT')}=${envContext} is set and overrides this switch. ` +
        `Unset it for "${name}" to take effect.`,
    )
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

async function renameAction(oldName: string, newName: string): Promise<void> {
  if (oldName === newName) {
    info('Nothing to do — old and new names are the same.')
    return
  }
  const result = await renameContext(oldName, newName)
  if (!result) {
    const contexts = await listContexts()
    if (!contexts.some((c) => c.name === oldName)) {
      errorOutput(`Context "${oldName}" not found.`)
      if (contexts.length > 0) {
        info(`Available: ${contexts.map((c) => c.name).join(', ')}`)
      }
    } else {
      errorOutput(`A context named "${newName}" already exists.`)
    }
    return
  }
  const envContext = process.env.TEMPS_CONTEXT?.trim() || null
  success(`Renamed context "${oldName}" → "${newName}"`)
  if (envContext === oldName) {
    warning(
      `${colors.bold('TEMPS_CONTEXT')}=${oldName} is set in your environment. ` +
        `Update it to "${newName}" for the change to take effect in this shell.`,
    )
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
    .command('rename <old-name> <new-name>')
    .description('Rename a context')
    .action(renameAction)

  ctx
    .command('current')
    .description('Print the active context name')
    .option('--json', 'Output in JSON format with full details')
    .action(currentAction)
}
