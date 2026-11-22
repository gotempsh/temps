// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
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
import {
  addValueSourceOptions,
  resolveValueSource,
  valueSourceInput,
  type ValueSourceIO,
} from '../../lib/value-source.js'

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

/** Where the value may come from: `-v`, `--value-file`, `--value-stdin`. */
interface ValueSourceOptions {
  value?: string
  valueFile?: string
  valueStdin?: boolean
  literal?: boolean
  trimTrailingNewline?: boolean
}

function hasValueSource(options: ValueSourceOptions): boolean {
  return options.value !== undefined || options.valueFile !== undefined || options.valueStdin === true
}

/**
 * Read the secret value from its one source. `-v @path` still reads a file
 * (with a deprecation note) unless `--literal` is passed. A newline hint is
 * shown only for env-type secrets: a file-type secret (a PEM key, a JSON
 * credentials file) normally ends in one.
 */
export async function resolveSecretValue(
  name: string,
  secretType: string,
  options: ValueSourceOptions,
  io?: ValueSourceIO,
): Promise<string> {
  const resolved = await resolveValueSource(
    valueSourceInput(options.value, options as Record<string, unknown>),
    {
      subject: `the value for secret ${name}`,
      flags: { inline: '-v/--value', file: '--value-file', stdin: '--value-stdin' },
      required: true,
      legacyAtPath: true,
      newlineHint: secretType !== 'file',
    },
    io,
  )
  // required: true guarantees a value.
  return resolved!.value
}

// --- Options ---

interface ListOptions {
  json?: boolean
}

interface CreateOptions extends ValueSourceOptions {
  name: string
  type?: string
  mountPath?: string
  description?: string
}

type UpdateOptions = ValueSourceOptions & {
  type?: string
  mountPath?: string
  description?: string
}

interface DeleteOptions {
  force?: boolean
  yes?: boolean
}

// --- Registration ---

const CREATE_EXAMPLES = `
Examples:
  # An env-type secret piped from a password manager
  $ op read --no-newline op://vault/api/key | temps secrets create -n my_api_key --value-stdin

  # A file-type secret, sent byte-for-byte
  $ temps secrets create -n gsc_creds -t file -m /home/temps/gsc.json --value-file ./gsc.json
`

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

  const create = secrets
    .command('create')
    .alias('add')
    .description('Create or update a secret (upsert by name). Prefer --value-file or --value-stdin, which keep the value out of shell history')
    .requiredOption('-n, --name <name>', 'Secret name')
    .option(
      '-v, --value <value>',
      'Secret value on the command line (visible in shell history and `ps`). `-v @path` reading a file is deprecated: use --value-file',
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
  addValueSourceOptions(create, { what: 'the secret value', legacyAtPath: true })
  create.addHelpText('after', CREATE_EXAMPLES).action(createAction)

  const update = secrets
    .command('update')
    .description('Update an existing secret (alias for create — upserts). The value must be re-supplied: the server never returns it')
    .requiredOption('-n, --name <name>', 'Secret name')
    .option(
      '-v, --value <value>',
      'New value on the command line (visible in shell history). `-v @path` is deprecated: use --value-file',
    )
    .option('-t, --type <type>', 'Secret type: "env" or "file"')
    .option('-m, --mount-path <path>', 'New mount path (file type only)')
    .option('-d, --description <description>', 'New description')
  addValueSourceOptions(update, { what: 'the new secret value', legacyAtPath: true })
  update.action(updateAction)

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

/** The type to create, validated before any value is read. */
export function createSecretType(options: Pick<CreateOptions, 'type' | 'mountPath'>): 'env' | 'file' {
  const secretType = (options.type ?? 'env').toLowerCase()
  if (secretType !== 'env' && secretType !== 'file') {
    throw new Error(`Invalid --type: must be "env" or "file"`)
  }
  if (secretType === 'file' && !options.mountPath) {
    throw new Error('--mount-path is required when --type is "file"')
  }
  return secretType
}

/** `value` is the already-resolved secret value, never a `@path`. */
export function buildCreateSecretBody(
  options: Pick<CreateOptions, 'name' | 'type' | 'mountPath' | 'description'> & { value: string },
): Record<string, unknown> {
  const secretType = createSecretType(options)
  const value = options.value

  const body: Record<string, unknown> = {
    name: options.name,
    secret_type: secretType,
    value,
  }
  if (options.mountPath) body.mount_path = options.mountPath
  if (options.description) body.description = options.description
  return body
}

const UPDATE_NEEDS_VALUE =
  'A value is required when updating (the server does not echo the existing value). ' +
  'Re-provide it with --value-file <path>, --value-stdin, or -v/--value.'

/** The type after the update, validated before any value is read. */
export function updateSecretType(options: Pick<UpdateOptions, 'type'>, current: SecretResponse): 'env' | 'file' {
  const secretType = (options.type ?? current.secret_type).toLowerCase()
  if (secretType !== 'env' && secretType !== 'file') {
    throw new Error(`Invalid --type: must be "env" or "file"`)
  }
  return secretType
}

/** `options.value` is the already-resolved secret value, never a `@path`. */
export function buildUpdateSecretBody(
  options: Pick<UpdateOptions, 'type' | 'mountPath' | 'description'> & { name: string; value?: string },
  current: SecretResponse,
): Record<string, unknown> {
  // Value must always be supplied on upsert since the server doesn't return it.
  if (options.value === undefined) {
    throw new Error(UPDATE_NEEDS_VALUE)
  }

  const secretType = updateSecretType(options, current)

  const body: Record<string, unknown> = {
    name: options.name,
    secret_type: secretType,
    value: options.value,
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
      '  env:  op read --no-newline op://vault/api/key | temps secrets create -n my_api_key --value-stdin',
    )
    info(
      '  file: temps secrets create -n gsc_creds -t file -m gsc.json --value-file ./gsc.json',
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
  // Validate and read the value before authenticating, so a bad file or an
  // empty pipe fails fast and without a network round trip.
  const secretType = createSecretType(options)
  const value = await resolveSecretValue(options.name, secretType, options)
  const body = buildCreateSecretBody({ ...options, value })

  await requireAuth()
  await setupClient()

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
  // The API is upsert-based, so update simply requires the full body.
  // Fetch current state first so partial updates don't clobber other fields.
  if (
    !hasValueSource(options) &&
    options.type === undefined &&
    options.mountPath === undefined &&
    options.description === undefined
  ) {
    warning(
      'No fields to update. Provide at least one of --value-file, --value-stdin, --value, --type, --mount-path, or --description',
    )
    return
  }
  if (!hasValueSource(options)) {
    throw new Error(UPDATE_NEEDS_VALUE)
  }

  await requireAuth()
  await setupClient()

  const current = await withSpinner('Fetching current secret...', async () => {
    const { data, error } = await client.get({ url: '/settings/secrets' })
    if (error) throw new Error(getErrorMessage(error))
    const list = data as ListResponse
    const found = list.items.find((s) => s.name === options.name)
    if (!found) throw new Error(`Secret '${options.name}' not found`)
    return found
  })

  const value = await resolveSecretValue(options.name, updateSecretType(options, current), options)
  const body = buildUpdateSecretBody({ ...options, value }, current)

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
