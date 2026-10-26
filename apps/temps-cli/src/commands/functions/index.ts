// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { readFile } from 'node:fs/promises'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createFunction,
  deleteFunction,
  getCapability,
  getFunction,
  getProjectBySlug,
  listFunctions,
  sleepFunction,
  wakeFunction,
} from '../../api/sdk.gen.js'
import type {
  CreateFunctionRequest,
  FunctionResponse,
  FunctionsCapabilityResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
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
  keyValue,
  formatRelativeTime,
} from '../../ui/output.js'

interface ProjectOption {
  project?: string
}

interface ListOptions extends ProjectOption {
  page?: string
  pageSize?: string
  json?: boolean
}

interface ShowOptions extends ProjectOption {
  json?: boolean
}

export interface CreateOptions extends ProjectOption {
  name?: string
  wasm?: string
  image?: string
  port?: string
  memory?: string
  idleTimeout?: string
  wakeTimeout?: string
  json?: boolean
}

interface RemoveOptions extends ProjectOption {
  force?: boolean
  yes?: boolean
}

interface StateOptions extends ProjectOption {
  json?: boolean
}

/**
 * Parse an optional integer flag. Returns `undefined` when the flag was not
 * given, and throws naming the flag when it is not an integer in `[min, max]`,
 * so a typo never reaches the server as `NaN` or a silently clamped value.
 */
export function parseIntFlag(
  flag: string,
  raw: string | undefined,
  min: number,
  max: number,
): number | undefined {
  if (raw === undefined) return undefined
  const trimmed = raw.trim()
  if (!/^-?\d+$/.test(trimmed)) {
    throw new Error(`${flag} must be an integer, got "${raw}"`)
  }
  const value = Number(trimmed)
  if (value < min || value > max) {
    throw new Error(`${flag} must be between ${min} and ${max}, got ${value}`)
  }
  return value
}

/**
 * Check the flags of `functions create` before any file is read or request
 * sent: a name, and exactly one artifact (`--wasm` or `--image`).
 */
export function validateCreateOptions(options: CreateOptions): void {
  if (!options.name || options.name.trim() === '') {
    throw new Error('--name is required (lowercase letters, digits and inner hyphens)')
  }
  const hasWasm = options.wasm !== undefined && options.wasm !== ''
  const hasImage = options.image !== undefined && options.image !== ''
  if (hasWasm === hasImage) {
    throw new Error(
      'Pass exactly one of --wasm <file> (a wasm32-wasip1 module) or --image <ref> (an OCI image serving HTTP)',
    )
  }
}

/**
 * Build the create request from validated flags. `wasmBytes` is the content
 * of the `--wasm` file, read by the caller; it is sent base64-encoded.
 */
export function buildCreateBody(
  options: CreateOptions,
  wasmBytes?: Uint8Array,
): CreateFunctionRequest {
  validateCreateOptions(options)
  const body: CreateFunctionRequest = { name: options.name!.trim() }
  if (options.image) {
    body.image = options.image.trim()
  } else {
    if (!wasmBytes || wasmBytes.length === 0) {
      throw new Error(`--wasm file ${options.wasm} is empty`)
    }
    body.wasm_base64 = Buffer.from(wasmBytes).toString('base64')
  }
  const port = parseIntFlag('--port', options.port, 1, 65535)
  if (port !== undefined) body.port = port
  const memory = parseIntFlag('--memory', options.memory, 1, 4096)
  if (memory !== undefined) body.memory_limit_mb = memory
  const idle = parseIntFlag('--idle-timeout', options.idleTimeout, 10, 86400)
  if (idle !== undefined) body.idle_timeout_seconds = idle
  const wake = parseIntFlag('--wake-timeout', options.wakeTimeout, 1, 300)
  if (wake !== undefined) body.wake_timeout_seconds = wake
  return body
}

/** One line per runtime: available, or why not. */
export function describeCapability(capability: FunctionsCapabilityResponse): string[] {
  const runtime = (name: string, r: { available: boolean; reason?: string | null }) =>
    r.available ? `${name}: available` : `${name}: unavailable${r.reason ? ` (${r.reason})` : ''}`
  return [runtime('WASM', capability.wasm), runtime('OCI image', capability.oci)]
}

export function registerFunctionsCommands(program: Command): void {
  const functions = program
    .command('functions')
    .alias('fn')
    .description('Manage scale-to-zero functions (WASM modules or OCI images) in a project')

  functions
    .command('capability')
    .description('Show whether this Temps instance can run functions, and what is missing')
    .option('--json', 'Output in JSON format')
    .action(capabilityAction)

  functions
    .command('list')
    .alias('ls')
    .description('List the functions of a project')
    .option('-p, --project <project>', 'Project slug')
    .option('--page <n>', 'Page number', '1')
    .option('--page-size <n>', 'Items per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  functions
    .command('show <function>')
    .description('Show a function (ID or name), including why it is not serving')
    .option('-p, --project <project>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action(showAction)

  functions
    .command('create')
    .description('Create a function from a WASM module or an OCI image')
    .option('-p, --project <project>', 'Project slug')
    .option('--name <name>', 'Function name; part of its hostname')
    .option('--wasm <file>', 'WASI preview 1 module (wasm32-wasip1) serving HTTP CGI-style')
    .option('--image <ref>', 'OCI image reference serving HTTP on --port')
    .option('--port <port>', 'Port the function serves on (default 8080)')
    .option('--memory <mb>', 'Memory limit in MiB (1-4096)')
    .option('--idle-timeout <seconds>', 'Stop after this many idle seconds (10-86400, default 300)')
    .option('--wake-timeout <seconds>', 'How long the first request waits for a start (1-300, default 30)')
    .option('--json', 'Output in JSON format')
    .action(createAction)

  functions
    .command('rm <function>')
    .alias('delete')
    .description('Delete a function (ID or name)')
    .option('-p, --project <project>', 'Project slug')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeAction)

  functions
    .command('sleep <function>')
    .description('Stop a running function now; the next request wakes it')
    .option('-p, --project <project>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action(sleepAction)

  functions
    .command('wake <function>')
    .description('Start a function now instead of on its next request')
    .option('-p, --project <project>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action(wakeAction)
}

async function resolveProjectId(flag?: string): Promise<number> {
  const resolved = await requireProjectSlug(flag)
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return data.id
}

/**
 * Resolve a function given by numeric ID or by name. Names are unique per
 * project, so a name lookup pages through the list until it finds it.
 */
async function resolveFunctionId(projectId: number, ref: string): Promise<number> {
  if (/^\d+$/.test(ref)) return Number(ref)
  for (let page = 1; ; page++) {
    const { data, error } = await listFunctions({
      client,
      path: { project_id: projectId },
      query: { page, page_size: 100 },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    const found = data.functions.find((f) => f.name === ref)
    if (found) return found.id
    if (page >= data.total_pages) break
  }
  throw new Error(`Function "${ref}" not found in project ${projectId}`)
}

function printFunction(fn: FunctionResponse): void {
  newline()
  header(`${icons.info} Function ${fn.name}`)
  keyValue('ID', fn.id)
  keyValue('State', statusBadge(fn.state))
  keyValue('Hostname', fn.hostname)
  keyValue('Artifact', fn.artifact_kind === 'oci' ? `image ${fn.image_reference ?? '-'}` : `wasm ${fn.wasm_digest ?? '-'}`)
  keyValue('Port', fn.port)
  keyValue('Memory', fn.memory_limit_mb ? `${fn.memory_limit_mb} MiB` : 'unlimited')
  keyValue('Idle timeout', `${fn.idle_timeout_seconds}s`)
  keyValue('Wake timeout', `${fn.wake_timeout_seconds}s`)
  if (fn.ready_reason) keyValue('Not serving', fn.ready_reason)
  if (fn.ready_message) keyValue('Detail', fn.ready_message)
  keyValue('Created', formatRelativeTime(fn.created_at))
  newline()
}

async function capabilityAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const capability = await withSpinner('Checking function support...', async () => {
    const { data, error } = await getCapability({ client })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(capability)
    return
  }
  newline()
  if (capability.configured) {
    success('Functions can run on this instance')
  } else {
    warning(`Functions cannot run yet: ${capability.reason ?? 'no runtime is available'}`)
    if (capability.setup_path) info(`Set it up in the console at ${capability.setup_path}`)
  }
  for (const line of describeCapability(capability)) info(line)
  newline()
}

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const page = parseIntFlag('--page', options.page, 1, Number.MAX_SAFE_INTEGER) ?? 1
  const pageSize = parseIntFlag('--page-size', options.pageSize, 1, 100) ?? 20
  const projectId = await resolveProjectId(options.project)
  const result = await withSpinner('Fetching functions...', async () => {
    const { data, error } = await listFunctions({
      client,
      path: { project_id: projectId },
      query: { page, page_size: pageSize },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} Functions (${result.total})`)
  if (result.functions.length === 0) {
    info('No functions in this project')
    info('Run: temps functions create --name hello --image registry.example.com/team/hello:1 --port 8080')
    newline()
    return
  }
  const columns: TableColumn<FunctionResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'State', key: 'state', color: (v) => statusBadge(v) },
    { header: 'Artifact', key: 'artifact_kind' },
    { header: 'Hostname', key: 'hostname', color: (v) => colors.muted(v) },
    { header: 'Created', accessor: (f) => formatRelativeTime(f.created_at) },
  ]
  printTable(result.functions, columns, { style: 'minimal' })
  if (result.total_pages > 1) info(`Page ${result.page} of ${result.total_pages}`)
  newline()
}

async function showAction(ref: string, options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const fn = await withSpinner('Fetching function...', async () => {
    const functionId = await resolveFunctionId(projectId, ref)
    const { data, error } = await getFunction({
      client,
      path: { project_id: projectId, function_id: functionId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(fn)
    return
  }
  printFunction(fn)
}

async function createAction(options: CreateOptions): Promise<void> {
  validateCreateOptions(options)
  await requireAuth()
  await setupClient()
  let wasmBytes: Uint8Array | undefined
  if (options.wasm) {
    try {
      wasmBytes = await readFile(options.wasm)
    } catch (e) {
      throw new Error(`Cannot read --wasm file ${options.wasm}: ${(e as Error).message}`)
    }
  }
  const body = buildCreateBody(options, wasmBytes)
  const projectId = await resolveProjectId(options.project)
  const fn = await withSpinner(`Creating function ${body.name}...`, async () => {
    const { data, error } = await createFunction({
      client,
      path: { project_id: projectId },
      body,
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(fn)
    return
  }
  success(`Function ${colors.primary(fn.name)} created`)
  printFunction(fn)
}

async function removeAction(ref: string, options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const functionId = await resolveFunctionId(projectId, ref)
  if (!(options.force || options.yes)) {
    const confirmed = await promptConfirm({
      message: `Delete function ${ref}? Its hostname stops answering.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Deleting function...', async () => {
    const { error } = await deleteFunction({
      client,
      path: { project_id: projectId, function_id: functionId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Function ${ref} deleted`)
}

async function sleepAction(ref: string, options: StateOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const result = await withSpinner('Stopping function...', async () => {
    const functionId = await resolveFunctionId(projectId, ref)
    const { data, error } = await sleepFunction({
      client,
      path: { project_id: projectId, function_id: functionId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  if (result.slept) {
    success(`Function ${colors.primary(result.function.name)} stopped; the next request wakes it`)
  } else {
    info(`Function ${result.function.name} was not running (state: ${result.function.state})`)
  }
}

async function wakeAction(ref: string, options: StateOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const fn = await withSpinner('Starting function...', async () => {
    const functionId = await resolveFunctionId(projectId, ref)
    const { data, error } = await wakeFunction({
      client,
      path: { project_id: projectId, function_id: functionId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(fn)
    return
  }
  success(`Function ${colors.primary(fn.name)} is ${fn.state}`)
}
