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

interface SecretResponse {
  id: number
  name: string
  secret_type: 'env' | 'file'
  value: string // always masked server-side
  mount_path: string | null
  description: string | null
  created_at: string
  updated_at: string
}

interface ListResponse {
  items: SecretResponse[]
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

// --- Options ---

interface ListOptions {
  json?: boolean
}

interface CreateOptions {
  name: string
  value: string
  type?: string
  mountPath?: string
  description?: string
}

interface UpdateOptions {
  value?: string
  type?: string
  mountPath?: string
  description?: string
}

interface DeleteOptions {
  force?: boolean
  yes?: boolean
}

// --- Registration ---

export function registerSecretsCommands(program: Command): void {
  const secrets = program
    .command('secrets')
    .alias('secret')
    .description(
      'Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.',
    )

  secrets
    .command('list')
    .alias('ls')
    .description('List all secrets (values are masked)')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  secrets
    .command('create')
    .alias('add')
    .description('Create or update a secret (upsert by name)')
    .requiredOption('-n, --name <name>', 'Secret name')
    .requiredOption(
      '-v, --value <value>',
      'Secret value. Prefix with @ to read from file (e.g. @./creds.json)',
    )
    .option(
      '-t, --type <type>',
      'Secret type: "env" (default) or "file"',
      'env',
    )
    .option(
      '-m, --mount-path <path>',
      'Absolute path inside sandbox where file-type secret is written (required for --type file)',
    )
    .option('-d, --description <description>', 'Human-readable description')
    .action(createAction)

  secrets
    .command('update')
    .description('Update an existing secret (alias for create — upserts)')
    .requiredOption('-n, --name <name>', 'Secret name')
    .option(
      '-v, --value <value>',
      'New value. Prefix with @ to read from file',
    )
    .option('-t, --type <type>', 'Secret type: "env" or "file"')
    .option('-m, --mount-path <path>', 'New mount path (file type only)')
    .option('-d, --description <description>', 'New description')
    .action(updateAction)

  secrets
    .command('delete')
    .alias('rm')
    .description('Delete a secret')
    .argument('<name>', 'Name of the secret to delete')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(deleteAction)
}

// --- Body builders ---

export function buildCreateSecretBody(options: CreateOptions): Record<string, unknown> {
  const secretType = (options.type ?? 'env').toLowerCase()
  if (secretType !== 'env' && secretType !== 'file') {
    throw new Error(`Invalid --type: must be "env" or "file"`)
  }
  if (secretType === 'file' && !options.mountPath) {
    throw new Error('--mount-path is required when --type is "file"')
  }

  const value = resolveValue(options.value)

  const body: Record<string, unknown> = {
    name: options.name,
    secret_type: secretType,
    value,
  }
  if (options.mountPath) body.mount_path = options.mountPath
  if (options.description) body.description = options.description
  return body
}

export function buildUpdateSecretBody(
  options: UpdateOptions & { name: string },
  current: SecretResponse,
): Record<string, unknown> {
  // Value must always be supplied on upsert since the server doesn't return it.
  if (options.value === undefined) {
    throw new Error(
      '--value is required when updating (the server does not echo the existing value). Re-provide it.',
    )
  }

  const secretType = (options.type ?? current.secret_type).toLowerCase()
  if (secretType !== 'env' && secretType !== 'file') {
    throw new Error(`Invalid --type: must be "env" or "file"`)
  }

  const body: Record<string, unknown> = {
    name: options.name,
    secret_type: secretType,
    value: resolveValue(options.value),
  }
  const mountPath = options.mountPath ?? current.mount_path ?? undefined
  if (mountPath) body.mount_path = mountPath
  const description = options.description ?? current.description ?? undefined
  if (description) body.description = description

  if (secretType === 'file' && !body.mount_path) {
    throw new Error('--mount-path is required when type is "file"')
  }

  return body
}

// --- Actions ---

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const items = await withSpinner('Fetching secrets...', async () => {
    const { data, error } = await client.get({ url: '/settings/secrets' })
    if (error) throw new Error(getErrorMessage(error))
    return data as ListResponse
  })

  if (options.json) {
    json(items)
    return
  }

  newline()
  header(`${icons.info} Secrets (${items.items.length})`)

  if (items.items.length === 0) {
    info('No secrets defined yet.')
    info('Examples:')
    info(
      '  env:  temps secrets create -n my_api_key -v "sk-..."',
    )
    info(
      '  file: temps secrets create -n gsc_creds -t file -m gsc.json -v @./gsc.json',
    )
    newline()
    return
  }

  for (const secret of items.items) {
    const typeBadge =
      secret.secret_type === 'file'
        ? colors.info('file')
        : colors.warning('env')
    console.log(
      `  ${colors.primary(secret.name)} [${typeBadge}]`,
    )
    if (secret.description) {
      console.log(`    ${colors.muted(secret.description)}`)
    }
    if (secret.secret_type === 'file' && secret.mount_path) {
      console.log(
        `    ${colors.muted('Mount path:')} ${colors.bold(secret.mount_path)}`,
      )
      console.log(
        `    ${colors.muted('Use in MCP env:')} point a path variable (e.g. GOOGLE_APPLICATION_CREDENTIALS) at ${colors.bold(secret.mount_path)}`,
      )
    } else {
      console.log(
        `    ${colors.muted('Reference:')} ${colors.bold(`\${TEMPS_SECRET:${secret.name}}`)}`,
      )
    }
    newline()
  }
}

async function createAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const body = buildCreateSecretBody(options)

  const secret = await withSpinner('Saving secret...', async () => {
    const { data, error } = await client.post({
      url: '/settings/secrets',
      body,
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as SecretResponse
  })

  success(`Secret saved: ${secret.name} (${secret.secret_type})`)
  if (secret.secret_type === 'file' && secret.mount_path) {
    info(
      `File-type secret: written to ${secret.mount_path} (mode 0o600) at workflow runtime`,
    )
    info(
      `In MCP config, point a path-valued env var at this mount path, e.g.:`,
    )
    info(`  "GOOGLE_APPLICATION_CREDENTIALS": "${secret.mount_path}"`)
  } else {
    info(
      `Env-type secret: reference in MCP config as \${TEMPS_SECRET:${secret.name}}`,
    )
    info(
      `  "env": { "MY_API_KEY": "\${TEMPS_SECRET:${secret.name}}" }`,
    )
  }
}

async function updateAction(options: UpdateOptions & { name: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  // The API is upsert-based, so update simply requires the full body.
  // Fetch current state first so partial updates don't clobber other fields.
  if (
    options.value === undefined &&
    options.type === undefined &&
    options.mountPath === undefined &&
    options.description === undefined
  ) {
    warning(
      'No fields to update. Provide at least one of --value, --type, --mount-path, or --description',
    )
    return
  }

  const current = await withSpinner('Fetching current secret...', async () => {
    const { data, error } = await client.get({ url: '/settings/secrets' })
    if (error) throw new Error(getErrorMessage(error))
    const list = data as ListResponse
    const found = list.items.find((s) => s.name === options.name)
    if (!found) throw new Error(`Secret '${options.name}' not found`)
    return found
  })

  const body = buildUpdateSecretBody(options, current)

  const secret = await withSpinner('Updating secret...', async () => {
    const { data, error } = await client.post({
      url: '/settings/secrets',
      body,
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as SecretResponse
  })

  success(`Secret updated: ${secret.name} (${secret.secret_type})`)
  if (secret.secret_type === 'file' && secret.mount_path) {
    info(
      `Re-written to ${secret.mount_path} on next workflow run (mode 0o600)`,
    )
  }
}

async function deleteAction(
  name: string,
  options: DeleteOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete secret "${name}"? This cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting secret...', async () => {
    const { error } = await client.delete({
      url: '/settings/secrets/{name}',
      path: { name },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Secret "${name}" deleted`)
}
