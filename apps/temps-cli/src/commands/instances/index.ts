import type { Command } from 'commander'
import {
  listInstances,
  addInstance,
  removeInstance,
  setDefaultInstance,
  getInstance,
} from '../../config/instances.js'
import { config, credentials } from '../../config/store.js'
import { promptText, promptConfirm, promptUrl } from '../../ui/prompts.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  success,
  info,
  warning,
  newline,
  header,
  icons,
  colors,
  keyValue,
  json as jsonOutput,
  error as errorOutput,
} from '../../ui/output.js'

interface TempsInstanceDisplay {
  name: string
  url: string
  email?: string
  isDefault?: boolean
}

async function listAction(options: { json?: boolean }): Promise<void> {
  const instances = await listInstances()

  if (instances.length === 0) {
    newline()
    info('No instances configured')
    info(`Add one with: ${colors.bold('temps login --cloud')}`)
    newline()
    return
  }

  if (options.json) {
    jsonOutput(instances.map(i => ({ name: i.name, url: i.url, email: i.email, isDefault: i.isDefault })))
    return
  }

  newline()
  header(`${icons.globe} Temps Instances (${instances.length})`)

  const columns: TableColumn<TempsInstanceDisplay>[] = [
    {
      header: '',
      accessor: (i) => i.isDefault ? '●' : '○',
      color: (value, i) => i.isDefault ? colors.success(value) : colors.muted(value),
    },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'URL', key: 'url', color: (v) => colors.primary(v) },
    {
      header: 'Email',
      accessor: (i) => i.email ?? '-',
      color: (value, i) => i.email ? value : colors.muted(value),
    },
  ]

  printTable(instances, columns, { style: 'minimal' })
  newline()
}

async function addAction(options: { name?: string; url?: string }): Promise<void> {
  const name = options.name ?? await promptText({
    message: 'Instance name',
    required: true,
  })

  const url = options.url ?? await promptUrl('Instance URL')

  await addInstance({ name, url, isDefault: false })
  success(`Instance "${name}" added`)
  info(`Login with: ${colors.bold(`temps login --cloud --url ${url}`)}`)
}

async function removeAction(name: string): Promise<void> {
  const instance = await getInstance(name)
  if (!instance) {
    errorOutput(`Instance "${name}" not found`)
    return
  }

  const confirmed = await promptConfirm({
    message: `Remove instance "${name}" (${instance.url})?`,
    default: false,
  })

  if (!confirmed) {
    info('Cancelled')
    return
  }

  const removed = await removeInstance(name)
  if (removed) {
    success(`Instance "${name}" removed`)
  } else {
    errorOutput(`Instance "${name}" not found`)
  }
}

async function switchAction(name: string): Promise<void> {
  const instance = await getInstance(name)
  if (!instance) {
    errorOutput(`Instance "${name}" not found`)
    const instances = await listInstances()
    if (instances.length > 0) {
      info(`Available: ${instances.map(i => i.name).join(', ')}`)
    }
    return
  }

  // Set as default
  await setDefaultInstance(name)

  // Update global config to point to this instance
  config.set('apiUrl', instance.url)

  // Swap credentials if available
  if (instance.apiKey) {
    await credentials.setAll({
      apiKey: instance.apiKey,
      email: instance.email,
    })
  }

  success(`Switched to instance "${name}"`)
  keyValue('URL', instance.url)
  if (instance.email) {
    keyValue('Email', instance.email)
  }
}

async function showAction(name: string | undefined, options: { json?: boolean }): Promise<void> {
  if (name) {
    const instance = await getInstance(name)
    if (!instance) {
      errorOutput(`Instance "${name}" not found`)
      return
    }

    if (options.json) {
      jsonOutput({ name: instance.name, url: instance.url, email: instance.email, isDefault: instance.isDefault })
      return
    }

    newline()
    header(`${icons.globe} ${instance.name}`)
    keyValue('URL', instance.url)
    keyValue('Email', instance.email ?? colors.muted('not set'))
    keyValue('Default', instance.isDefault ? 'Yes' : 'No')
    newline()
  } else {
    // Show current instance info
    const apiUrl = config.get('apiUrl')
    const email = await credentials.get('email')

    if (options.json) {
      jsonOutput({ url: apiUrl, email })
      return
    }

    newline()
    header(`${icons.globe} Current Instance`)
    keyValue('URL', apiUrl)
    keyValue('Email', email ?? colors.muted('not logged in'))
    newline()
  }
}

export function registerInstancesCommands(program: Command): void {
  const instances = program
    .command('instances')
    .alias('instance')
    .description('Manage Temps server instances')

  instances
    .command('list')
    .alias('ls')
    .description('List configured instances')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  instances
    .command('add')
    .description('Add a new instance')
    .option('-n, --name <name>', 'Instance name')
    .option('-u, --url <url>', 'Instance URL')
    .action(addAction)

  instances
    .command('remove <name>')
    .alias('rm')
    .description('Remove an instance')
    .action(removeAction)

  instances
    .command('switch <name>')
    .alias('use')
    .description('Switch to a different instance')
    .action(switchAction)

  instances
    .command('show [name]')
    .description('Show instance details (or current instance)')
    .option('--json', 'Output in JSON format')
    .action(showAction)
}
