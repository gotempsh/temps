// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps kv`: a project's keys in the Temps KV store (Redis-backed, the same
 * store `@temps-sdk/kv` reads), and the instance-wide service behind it.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  kvDel,
  kvDisable,
  kvEnable,
  kvExpire,
  kvGet,
  kvGetSettings,
  kvIncr,
  kvKeys,
  kvSet,
  kvStatus,
  kvTtl,
  kvUpdate,
  kvUpdateSettings,
} from '../../api/sdk.gen.js'
import type { EnableKvRequest, SetRequest } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'
import { parseIntegerFlag, resolveProjectFlag } from '../../lib/project-id.js'
import { addValueSourceOptions, resolveValueSource, valueSourceInput } from '../../lib/value-source.js'
import { containerNameRequest, printContainerNameSettings, printManagedServiceStatus } from '../../lib/managed-service.js'

const PROJECT_FLAG = '-p, --project <slug-or-id>'
const PROJECT_HELP = 'Project slug or ID (defaults to the linked project)'

interface ProjectOptions {
  project?: string
  json?: boolean
}

export interface SetOptions extends ProjectOptions {
  valueFile?: string
  valueStdin?: boolean
  trimTrailingNewline?: boolean
  jsonValue?: boolean
  ex?: string
  px?: string
  nx?: boolean
  xx?: boolean
}

interface ConfirmOptions {
  yes?: boolean
}

export interface EnableOptions {
  image?: string
  maxMemory?: string
  persistence?: boolean
  json?: boolean
}

/** A stored value as typed on the command line: a string, or JSON with `--json-value`. */
export function parseKvValue(raw: string, asJson: boolean | undefined): unknown {
  if (!asJson) return raw
  try {
    return JSON.parse(raw)
  } catch (err) {
    throw new ValidationError(`--json-value was passed but the value is not JSON: ${(err as Error).message}`)
  }
}

/** The `SetRequest` options besides key and value: expiry and the NX/XX conditions. */
export function buildSetOptions(options: SetOptions): Pick<SetRequest, 'ex' | 'px' | 'nx' | 'xx'> {
  if (options.ex !== undefined && options.px !== undefined) {
    throw new ValidationError('Pass --ex (seconds) or --px (milliseconds), not both')
  }
  if (options.nx && options.xx) {
    throw new ValidationError('Pass --nx (only if absent) or --xx (only if present), not both')
  }
  const result: Pick<SetRequest, 'ex' | 'px' | 'nx' | 'xx'> = {}
  if (options.ex !== undefined) result.ex = parseIntegerFlag(options.ex, '--ex', 1)
  if (options.px !== undefined) result.px = parseIntegerFlag(options.px, '--px', 1)
  if (options.nx) result.nx = true
  if (options.xx) result.xx = true
  return result
}

/** The enable request from flags; every field is optional. */
export function buildEnableRequest(options: EnableOptions): EnableKvRequest {
  const body: EnableKvRequest = {}
  if (options.image !== undefined) body.docker_image = options.image
  if (options.maxMemory !== undefined) {
    if (!/^\d+(b|kb|mb|gb)?$/i.test(options.maxMemory.trim())) {
      throw new ValidationError(`--max-memory takes a size such as 256mb or 1gb, got "${options.maxMemory}"`)
    }
    body.max_memory = options.maxMemory.trim()
  }
  if (options.persistence !== undefined) body.persistence = options.persistence
  return body
}

/** What a TTL means, as `kv ttl` prints it. */
export function describeTtl(ttl: number): string {
  if (ttl === -2) return 'the key does not exist'
  if (ttl === -1) return 'no expiration'
  return `${ttl}s`
}

function fail(what: string, error: unknown): never {
  throw new Error(`${what}: ${getErrorMessage(error)}`)
}

export function registerKvCommands(program: Command): void {
  const kv = program
    .command('kv')
    .description("Read and write a project's keys in the KV store, and manage the KV service")

  kv.command('get <key>')
    .description('Get the value of a key')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(getAction)

  const set = kv
    .command('set <key> [value]')
    .description('Set a key; the value is a string unless --json-value')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json-value', 'Parse the value as JSON (a number, object, array, true/false or null)')
    .option('--ex <seconds>', 'Expire the key after this many seconds')
    .option('--px <milliseconds>', 'Expire the key after this many milliseconds')
    .option('--nx', 'Only set the key if it does not exist')
    .option('--xx', 'Only set the key if it already exists')
    .option('--json', 'Output in JSON format')
  addValueSourceOptions(set, { what: 'the value' })
  set.action(setAction)

  kv.command('del <keys...>')
    .alias('delete')
    .description('Delete one or more keys')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(delAction)

  kv.command('keys [pattern]')
    .alias('ls')
    .description('List keys matching a pattern (* and ? wildcards; default *)')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(keysAction)

  kv.command('ttl <key>')
    .description('Show how long until a key expires')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(ttlAction)

  kv.command('expire <key> <seconds>')
    .description('Expire a key after a number of seconds')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(expireAction)

  kv.command('incr <key>')
    .description('Increment a numeric key (creating it at 0 first)')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--by <amount>', 'Amount to add, may be negative (default 1)')
    .option('--json', 'Output in JSON format')
    .action(incrAction)

  // The instance-wide service.
  kv.command('status')
    .description('Show whether the KV service is enabled and healthy on this instance')
    .option('--json', 'Output in JSON format')
    .action(statusAction)

  kv.command('enable')
    .description('Enable the KV service on this instance (starts its Redis container)')
    .option('--image <ref>', 'Docker image to run (default: the one this Temps release pins)')
    .option('--max-memory <size>', 'Memory limit, e.g. 256mb or 1gb')
    .option('--persistence', 'Persist data to disk')
    .option('--no-persistence', 'Keep data in memory only')
    .option('--json', 'Output in JSON format')
    .action(enableAction)

  kv.command('update')
    .description('Change the Docker image the KV service runs (recreates its container)')
    .requiredOption('--image <ref>', 'Docker image to run')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(updateAction)

  kv.command('disable')
    .description('Disable the KV service on this instance (stops its container; every project loses KV access)')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(disableAction)

  kv.command('container-name')
    .description('Show, or set with --container-name/--reset, the Docker container name of the KV service')
    .option('--container-name <name>', 'New container name (must keep the required prefix)')
    .option('--reset', 'Go back to the default container name')
    .option('--json', 'Output in JSON format')
    .action(containerNameAction)
}

async function prepare(project: string | undefined): Promise<number> {
  await requireAuth()
  await setupClient()
  return (await resolveProjectFlag(project)).id
}

async function getAction(key: string, options: ProjectOptions): Promise<void> {
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Reading ${key}...`, async () => {
    const { data, error } = await kvGet({ client, body: { key, project_id: projectId } })
    if (error || !data) fail(`Could not read key "${key}"`, error)
    return data
  })
  // `exists` tells a stored JSON null from a missing key; a server that
  // predates it only has the value, where both read as null.
  const exists = typeof result.exists === 'boolean' ? result.exists : result.value !== null && result.value !== undefined
  if (!exists) process.exitCode = 1
  if (options.json) {
    json({ key, value: result.value ?? null, exists })
    return
  }
  if (!exists) {
    info(`Key "${key}" does not exist`)
    return
  }
  console.log(typeof result.value === 'string' ? result.value : JSON.stringify(result.value, null, 2))
}

async function setAction(key: string, inline: string | undefined, options: SetOptions): Promise<void> {
  const conditions = buildSetOptions(options)
  const resolved = await resolveValueSource(valueSourceInput(inline, options as Record<string, unknown>), {
    subject: `the value for ${key}`,
    flags: { inline: '[value]', file: '--value-file', stdin: '--value-stdin' },
    required: true,
  })
  const value = parseKvValue(resolved?.value ?? '', options.jsonValue)
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Setting ${key}...`, async () => {
    const { data, error } = await kvSet({ client, body: { key, value, project_id: projectId, ...conditions } })
    if (error || !data) fail(`Could not set key "${key}"`, error)
    return data
  })
  // With --nx/--xx a condition that does not hold is not an error from Redis; say so.
  if (result.result !== 'OK') process.exitCode = 1
  if (options.json) {
    json({ key, ...result })
    return
  }
  if (result.result !== 'OK') {
    warning(`Key "${key}" was not set (${options.nx ? 'it already exists' : options.xx ? 'it does not exist' : result.result})`)
    return
  }
  success(`Set ${key}`)
}

async function delAction(keys: string[], options: ProjectOptions & ConfirmOptions): Promise<void> {
  const projectId = await prepare(options.project)
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete ${keys.length === 1 ? `key "${keys[0]}"` : `${keys.length} keys`}? This cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled; nothing was deleted.')
      return
    }
  }
  const result = await withSpinner('Deleting...', async () => {
    const { data, error } = await kvDel({ client, body: { keys, project_id: projectId } })
    if (error || !data) fail(`Could not delete ${keys.join(', ')}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Deleted ${result.deleted} of ${keys.length} key(s)`)
}

async function keysAction(pattern: string | undefined, options: ProjectOptions): Promise<void> {
  const projectId = await prepare(options.project)
  const match = pattern ?? '*'
  const result = await withSpinner('Listing keys...', async () => {
    const { data, error } = await kvKeys({ client, body: { pattern: match, project_id: projectId } })
    if (error || !data) fail(`Could not list keys matching "${match}"`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} Keys matching ${colors.bold(match)} (${result.keys.length})`)
  if (result.keys.length === 0) info('No keys match')
  for (const key of result.keys) console.log(key)
  newline()
}

async function ttlAction(key: string, options: ProjectOptions): Promise<void> {
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Reading the TTL of ${key}...`, async () => {
    const { data, error } = await kvTtl({ client, body: { key, project_id: projectId } })
    if (error || !data) fail(`Could not read the TTL of "${key}"`, error)
    return data
  })
  if (options.json) {
    json({ key, ...result })
    return
  }
  info(`${key}: ${describeTtl(result.ttl)}`)
}

async function expireAction(key: string, rawSeconds: string, options: ProjectOptions): Promise<void> {
  const seconds = parseIntegerFlag(rawSeconds, '<seconds>', 1)
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Setting the expiry of ${key}...`, async () => {
    const { data, error } = await kvExpire({ client, body: { key, seconds, project_id: projectId } })
    if (error || !data) fail(`Could not set the expiry of "${key}"`, error)
    return data
  })
  if (!result.success) process.exitCode = 1
  if (options.json) {
    json({ key, ...result })
    return
  }
  if (!result.success) {
    warning(`Key "${key}" does not exist; nothing to expire`)
    return
  }
  success(`${key} expires in ${seconds}s`)
}

async function incrAction(key: string, options: ProjectOptions & { by?: string }): Promise<void> {
  const amount = options.by === undefined ? undefined : parseIntegerFlag(options.by, '--by', Number.MIN_SAFE_INTEGER)
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Incrementing ${key}...`, async () => {
    const { data, error } = await kvIncr({
      client,
      body: { key, project_id: projectId, ...(amount !== undefined && { amount }) },
    })
    if (error || !data) fail(`Could not increment "${key}"`, error)
    return data
  })
  if (options.json) {
    json({ key, ...result })
    return
  }
  console.log(String(result.value))
}

async function statusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Fetching KV status...', async () => {
    const { data, error } = await kvStatus({ client })
    if (error || !data) fail('Could not read the KV service status', error)
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  printManagedServiceStatus('KV service', 'kv', status)
}

async function enableAction(options: EnableOptions): Promise<void> {
  const body = buildEnableRequest(options)
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Enabling the KV service...', async () => {
    const { data, error } = await kvEnable({ client, body })
    if (error || !data) fail('Could not enable the KV service', error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
  info('Check it with: temps kv status')
}

async function updateAction(options: { image: string; json?: boolean } & ConfirmOptions): Promise<void> {
  const image = options.image.trim()
  if (!image) throw new ValidationError('--image needs an image reference')
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    warning('Changing the image recreates the KV container; projects cannot reach KV until it is back.')
    if (!(await promptConfirm({ message: `Run the KV service on ${image}?`, default: false }))) {
      info('Cancelled; nothing was changed.')
      return
    }
  }
  const result = await withSpinner(`Updating the KV service to ${image}...`, async () => {
    const { data, error } = await kvUpdate({ client, body: { docker_image: image } })
    if (error || !data) fail(`Could not update the KV service to ${image}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
}

async function disableAction(options: ConfirmOptions & { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    warning('Disabling stops the KV service: every project using @temps-sdk/kv loses access until it is enabled again.')
    if (!(await promptConfirm({ message: 'Disable the KV service?', default: false }))) {
      info('Cancelled; nothing was changed.')
      return
    }
  }
  const result = await withSpinner('Disabling the KV service...', async () => {
    const { data, error } = await kvDisable({ client })
    if (error || !data) fail('Could not disable the KV service', error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
}

async function containerNameAction(options: { containerName?: string; reset?: boolean; json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const { data: current, error: readError } = await kvGetSettings({ client })
  if (readError || !current) fail('Could not read the KV container-name setting', readError)
  if (options.containerName === undefined && !options.reset) {
    if (options.json) json(current)
    else printContainerNameSettings('KV container name', current)
    return
  }
  const body = containerNameRequest(options, current.required_prefix)
  const saved = await withSpinner('Saving the KV container name...', async () => {
    const { data, error } = await kvUpdateSettings({ client, body })
    if (error || !data) fail('Could not save the KV container name', error)
    return data
  })
  if (options.json) {
    json(saved)
    return
  }
  success(`KV container name: ${saved.container_name}`)
}
