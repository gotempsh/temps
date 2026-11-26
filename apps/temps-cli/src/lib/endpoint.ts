// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * One command per API operation, declared as a typed table entry.
 *
 * Most of the API is plain CRUD: a path, some query parameters, a JSON body
 * and a JSON reply. Hand-writing a command for each would be hundreds of
 * near-identical files, so a command area declares the operation instead:
 *
 * ```ts
 * endpoint(backups, listBackupSchedules, {
 *   name: 'schedules list',
 *   description: 'List backup schedules',
 *   query: { page: 'int', per_page: 'int' },
 * })
 * ```
 *
 * and gets a command with a positional argument per path parameter, a
 * `--kebab-flag` per query parameter and body field, `--from-file`/`--set`
 * for nested bodies, `--json`, `--yes` on risky operations, `-o` for
 * downloads, and JSON lines for event streams.
 *
 * The tables are typed against the generated SDK: `path`, `query` and
 * `body` must name every parameter and field of the operation
 * (`Record<keyof …>`), a required field must be marked `!`, and a kind must
 * fit the field's type, so an API change that adds a field fails the
 * typecheck until the command can set it.
 */

import type { Command, Option } from 'commander'
import { client } from './api-client.js'
import { call, confirmRisk, connect, output, printList } from './command-kit.js'
import { applyAssignments, collect, parseAssignment, readJsonObject, type JsonObject } from './json-edit.js'
import { parseIntegerFlag, resolveProjectFlag } from './project-id.js'
import { trimOneTrailingNewline } from './value-source.js'
import { ValidationError } from '../utils/errors.js'
import WebSocket from 'ws'
import { credentials } from '../config/store.js'
import { readRejectionBody, runSession, terminalSize } from '../commands/sandbox/shell.js'
import { isQuietMode, setQuietMode } from '../ui/spinner.js'
import { success } from '../ui/output.js'

/** How a field is read from the command line. */
export type Scalar = 'string' | 'secret' | 'int' | 'number' | 'bool' | 'json' | 'strings' | 'ints' | 'file'

type Present<T> = Exclude<T, null | undefined>

/** The kinds that fit a field of type `T`. */
type KindsFor<T> = [Present<T>] extends [never]
  ? 'json'
  : unknown extends T
    ? 'json'
    : Present<T> extends Blob | File
      ? 'file'
      : Present<T> extends boolean
        ? 'bool' | 'json'
        : Present<T> extends number
          ? 'int' | 'number' | 'json'
          : Present<T> extends string
            ? 'string' | 'secret' | 'json'
            : Present<T> extends readonly string[]
              ? 'strings' | 'json'
              : Present<T> extends readonly number[]
                ? 'ints' | 'json'
                : 'json'

/** A required field's kind ends in `!`. */
type KindString<T, Optional extends boolean> = Optional extends true ? KindsFor<T> : `${KindsFor<T> & string}!`

type FieldSpec<T, Optional extends boolean> =
  | KindString<T, Optional>
  | {
      kind: KindString<T, Optional>
      /** Shown in `--help`. */
      help?: string
      /** The allowed values (an enum). */
      values?: readonly Extract<Present<T> extends readonly (infer E)[] ? E : Present<T>, string | number>[]
    }

/** Every key of every member of a union (`keyof (A | B)` is only the shared ones). */
type KeysOf<U> = U extends unknown ? keyof U : never
type ValueOf<U, K extends PropertyKey> = U extends unknown ? (K extends keyof U ? U[K] : never) : never
/** `true` when `K` may be left out in some member of `U`. */
type IsOptional<U, K extends PropertyKey> = true extends (
  U extends unknown ? (K extends keyof U ? ({} extends Pick<U, K> ? true : false) : true) : never
)
  ? true
  : false

/** A field table: every field of `B`, each with a kind that fits its type. */
export type Fields<B> = { [K in KeysOf<B> & string]: FieldSpec<ValueOf<B, K>, IsOptional<B, K>> }

type PathKind<T> = Present<T> extends number ? 'int' | 'project' : 'string'

type Part<O, K extends 'path' | 'query' | 'body'> = O extends { [P in K]?: infer V } ? Present<V> : never

type OptionsOf<F> = F extends (options: infer O) => unknown ? Present<O> : never

type LooseFields = Record<string, string | { kind: string; help?: string; values?: readonly (string | number)[] }>

/** Non-distributive: a union body (`oneOf`) is one table naming every variant's fields. */
type BodySpec<B> = [B] extends [never]
  ? never
  : [B] extends [Blob | File | string]
    ? '@raw'
    : [B] extends [readonly unknown[]]
      ? '@json'
      : string extends KeysOf<B>
        ? '@json' | LooseFields
        : Fields<B> | '@json'

/**
 * How the reply is printed: `bytes` to `-o` (stdout by default), `events`
 * (SSE) as JSON lines, `socket` (a WebSocket stream) one message per line,
 * `terminal` (a PTY WebSocket) attached to this terminal.
 */
export type OutputKind = 'json' | 'bytes' | 'events' | 'socket' | 'terminal'

interface BaseSpec {
  /** Words under the parent command: `'list'`, or `'schedules list'` for a subgroup. */
  name: string
  description: string
  /** Ask before running (refused without a terminal unless `--yes`). */
  confirm?: string
  output?: OutputKind
}

/** A table is required exactly when the operation has that part, so none can be forgotten. */
type PartSpec<K extends string, V> = [V] extends [never] ? { [P in K]?: never } : { [P in K]: V }

/**
 * - `path`: every path parameter, in URL order; `'project'` reads
 *   `-p/--project` (slug or ID, or the linked project).
 * - `query`: every query parameter.
 * - `body`: every body field, `'@json'` for a body given whole as JSON,
 *   `'@raw'` for a file upload.
 */
export type EndpointSpec<O> = BaseSpec &
  PartSpec<'path', [Part<O, 'path'>] extends [never] ? never : { [K in keyof Part<O, 'path'> & string]: PathKind<Part<O, 'path'>[K]> }> &
  PartSpec<'query', [Part<O, 'query'>] extends [never] ? never : Fields<Part<O, 'query'>>> &
  PartSpec<'body', BodySpec<Part<O, 'body'>>>

interface NormalizedField {
  name: string
  kind: Scalar
  required: boolean
  help?: string
  values?: readonly (string | number)[]
  option?: Option
  /** For a secret: the `--x-stdin` and `--x-file` options. */
  stdinOption?: Option
  fileOption?: Option
}

const RESERVED_FLAGS = new Set(['json', 'yes', 'project', 'output', 'input', 'from-file', 'set', 'body', 'help'])

/** `retention_days` / `retentionDays` → `retention-days`. */
export function flagName(field: string): string {
  return field
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .replace(/[_\s.]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .toLowerCase()
}

/** `public_id` → `<public-id>`. */
function argName(field: string): string {
  return `<${flagName(field)}>`
}

function normalizeFields(table: Record<string, unknown> | undefined): NormalizedField[] {
  if (!table) return []
  return Object.entries(table).map(([name, raw]) => {
    const spec = typeof raw === 'string' ? { kind: raw } : (raw as { kind: string; help?: string; values?: readonly (string | number)[] })
    const required = spec.kind.endsWith('!')
    return { name, kind: spec.kind.replace(/!$/, '') as Scalar, required, help: spec.help, values: spec.values }
  })
}

function placeholder(field: NormalizedField): string {
  switch (field.kind) {
    case 'int':
    case 'ints':
      return '<n>'
    case 'number':
      return '<number>'
    case 'json':
      return '<json>'
    case 'file':
      return '<path>'
    default:
      return '<value>'
  }
}

function describeField(field: NormalizedField, where: 'body' | 'query'): string {
  const parts = [field.help ?? `${where === 'body' ? 'Body field' : 'Query parameter'} ${field.name}`]
  if (field.values?.length) parts.push(`(one of: ${field.values.join(', ')})`)
  if (field.kind === 'strings' || field.kind === 'ints') parts.push('(repeatable)')
  if (field.kind === 'json') parts.push('(JSON)')
  if (field.required) parts.push('[required]')
  return parts.join(' ')
}

function addFieldOptions(
  cmd: Command,
  fields: NormalizedField[],
  where: 'body' | 'query',
  taken: Set<string>,
  onConflict: 'prefix' | 'throw' = 'prefix',
): void {
  for (const field of fields) {
    let flag = flagName(field.name)
    if (RESERVED_FLAGS.has(flag) || taken.has(flag)) {
      if (onConflict === 'throw') throw new Error(`"${cmd.name()}" already has --${flag}; it cannot also set ${where} field ${field.name}`)
      flag = `${where}-${flag}`
    }
    if (taken.has(flag)) throw new Error(`Two inputs of "${cmd.name()}" map to the flag --${flag}`)
    taken.add(flag)
    const description = describeField(field, where)
    if (field.kind === 'bool') {
      cmd.option(`--${flag}`, description)
      cmd.option(`--no-${flag}`, `Set ${field.name} to false`)
      field.option = cmd.options.find((o) => o.long === `--${flag}`)
      continue
    }
    if (field.kind === 'secret') {
      cmd.option(`--${flag}-stdin`, `${description} (read from stdin)`)
      cmd.option(`--${flag}-file <path>`, `${description} (read from a file)`)
      field.stdinOption = cmd.options.find((o) => o.long === `--${flag}-stdin`)
      field.fileOption = cmd.options.find((o) => o.long === `--${flag}-file`)
      continue
    }
    if (field.kind === 'strings' || field.kind === 'ints') {
      cmd.option(`--${flag} ${placeholder(field)}`, description, collect)
    } else {
      cmd.option(`--${flag} ${placeholder(field)}`, description)
    }
    field.option = cmd.options.find((o) => o.long === `--${flag}`)
  }
}

function parseNumber(raw: string, flag: string): number {
  const value = Number(raw.trim())
  if (raw.trim() === '' || !Number.isFinite(value)) throw new ValidationError(`${flag} must be a number, got "${raw}"`)
  return value
}

function parseJsonFlag(raw: string, flag: string): unknown {
  try {
    return JSON.parse(raw)
  } catch (err) {
    throw new ValidationError(`${flag} must be JSON: ${(err as Error).message}`)
  }
}

async function readStdinText(what: string): Promise<string> {
  const chunks: Buffer[] = []
  let size = 0
  for await (const chunk of process.stdin as AsyncIterable<Uint8Array | string>) {
    const buffer = Buffer.from(chunk)
    size += buffer.length
    if (size > 1_048_576) throw new ValidationError(`${what} on stdin is larger than 1 MiB`)
    chunks.push(buffer)
  }
  return trimOneTrailingNewline(Buffer.concat(chunks).toString('utf8'))
}

/** The value of one field from parsed options; `undefined` when not given. */
async function fieldValue(field: NormalizedField, options: Record<string, unknown>): Promise<unknown> {
  if (field.kind === 'secret') {
    const fromStdin = field.stdinOption && options[field.stdinOption.attributeName()]
    const file = field.fileOption && (options[field.fileOption.attributeName()] as string | undefined)
    if (fromStdin && file) throw new ValidationError(`Pass ${field.stdinOption?.long} or ${field.fileOption?.long}, not both`)
    if (fromStdin) return readStdinText(field.name)
    if (file !== undefined) {
      const source = Bun.file(file)
      if (!(await source.exists())) throw new ValidationError(`${field.fileOption?.long}: file not found: ${file}`)
      return trimOneTrailingNewline(await source.text())
    }
    return undefined
  }
  const option = field.option
  if (!option) return undefined
  const raw = options[option.attributeName()]
  if (raw === undefined) return undefined
  const flag = option.long ?? field.name
  let value: unknown
  switch (field.kind) {
    case 'bool':
      value = Boolean(raw)
      break
    case 'int':
      value = parseIntegerFlag(String(raw), flag, Number.MIN_SAFE_INTEGER)
      break
    case 'number':
      value = parseNumber(String(raw), flag)
      break
    case 'ints':
      value = (raw as string[]).map((item) => parseIntegerFlag(item, flag, Number.MIN_SAFE_INTEGER))
      break
    case 'strings':
      value = raw as string[]
      break
    case 'json':
      value = parseJsonFlag(String(raw), flag)
      break
    case 'file': {
      const path = String(raw)
      const source = path === '-' ? Bun.stdin : Bun.file(path)
      if (path !== '-' && !(await source.exists())) throw new ValidationError(`${flag}: file not found: ${path}`)
      value = source
      break
    }
    default:
      value = String(raw)
  }
  if (field.values?.length) {
    const items = Array.isArray(value) ? value : [value]
    for (const item of items) {
      if (!field.values.includes(item as string | number)) {
        throw new ValidationError(`${flag} must be one of ${field.values.join(', ')}, got "${String(item)}"`)
      }
    }
  }
  return value
}

export interface FieldFlags<T> {
  /** Add the flags to a command (throws if it already has one of them). */
  addTo<C extends Command>(cmd: C): C
  /** The fields passed on the command line, to merge into the request. */
  read(options: object): Promise<Partial<T>>
}

/**
 * Flags for request fields a hand-written command does not set itself, typed
 * like an endpoint table (`Partial`: name only the fields it lacks):
 *
 * ```ts
 * const CREATE_EXTRA = fieldFlags<CreateUserRequest>({ must_change_password: 'bool' })
 * CREATE_EXTRA.addTo(users.command('create'))
 * // in the action
 * body = { ...body, ...(await CREATE_EXTRA.read(options)) }
 * ```
 */
export function fieldFlags<T>(fields: Partial<Fields<T>>, where: 'body' | 'query' = 'body'): FieldFlags<T> {
  const normalized = normalizeFields(fields as Record<string, unknown>)
  return {
    addTo(cmd) {
      const taken = new Set(cmd.options.flatMap((o) => (o.long ? [o.long.replace(/^--(no-)?/, '')] : [])))
      addFieldOptions(cmd, normalized, where, taken, 'throw')
      return cmd
    },
    async read(options) {
      const values: Record<string, unknown> = {}
      for (const field of normalized) {
        const value = await fieldValue(field, options as Record<string, unknown>)
        if (value !== undefined) values[field.name] = value
      }
      return values as Partial<T>
    },
  }
}

/** Find the subcommand `name` of `parent`, or create it as a group. */
export function subgroup(parent: Command, name: string, description?: string): Command {
  const existing = parent.commands.find((c) => c.name() === name || c.aliases().includes(name))
  if (existing) return existing
  const group = parent.command(name)
  if (description) group.description(description)
  return group
}

/** The top-level command `name` (created when no area registered it). */
export function topGroup(program: Command, name: string, description: string): Command {
  return subgroup(program, name, description)
}

function humanize(words: string): string {
  return words.charAt(0).toUpperCase() + words.slice(1).replace(/-/g, ' ')
}

/** The JSON a list command prints as a list, or `undefined` for a single object. */
function listItems(value: unknown): unknown[] | undefined {
  if (Array.isArray(value)) return value
  return undefined
}

async function socketHeaders(): Promise<Record<string, string>> {
  const apiKey = await credentials.getApiKey()
  if (!apiKey) throw new Error('Not authenticated. Run `temps login` first.')
  // The token goes in a header, never the URL: URLs end up in proxy logs.
  return { Authorization: `Bearer ${apiKey}` }
}

/** Print every message of a WebSocket stream, one per line, until it closes or Ctrl+C. */
async function streamSocket(url: string, what: string): Promise<void> {
  const socket = new WebSocket(url, { headers: await socketHeaders(), handshakeTimeout: 30_000 })
  await new Promise<void>((resolve, reject) => {
    const onSigint = () => socket.close(1000)
    process.once('SIGINT', onSigint)
    const done = (error?: Error) => {
      process.removeListener('SIGINT', onSigint)
      if (error) reject(error)
      else resolve()
    }
    socket.on('message', (data: Buffer, isBinary: boolean) => {
      if (isBinary) process.stdout.write(data)
      else console.log(data.toString().replace(/\r?\n$/, ''))
    })
    socket.on('unexpected-response', (_req, res) => {
      const status = res.statusCode ?? 0
      void readRejectionBody(res).then((body) => {
        res.destroy()
        done(new Error(`${what}: HTTP ${status}${body ? `: ${body.slice(0, 500)}` : ''}`))
      })
    })
    socket.on('error', (error: Error) => done(new Error(`${what}: ${error.message}`)))
    socket.on('close', (code: number, reason: Buffer) => {
      if (code !== 1000 && code !== 1005 && code !== 1006) {
        done(new Error(`${what}: the stream closed with code ${code}${reason.length ? `: ${reason.toString()}` : ''}`))
      } else done()
    })
  })
}

/** Attach this terminal to a PTY WebSocket (same protocol as `temps sandbox shell`). */
async function attachTerminal(url: string, label: string): Promise<void> {
  if (!process.stdin.isTTY) throw new ValidationError(`${label} needs an interactive terminal`)
  const thread = new URL(url).searchParams.get('thread') ?? undefined
  const socket = new WebSocket(url, { headers: await socketHeaders(), handshakeTimeout: 120_000 })
  await runSession(socket, 'the workspace', { tab: thread }, 'Check the workspace exists and that you may open terminals in it (sandboxes:exec).')
}

async function printEvents(stream: AsyncGenerator<unknown>, events: { event?: string; id?: string }[]): Promise<void> {
  let index = 0
  for await (const data of stream) {
    const meta = events[index++] ?? {}
    console.log(JSON.stringify({ ...(meta.event && { event: meta.event }), ...(meta.id && { id: meta.id }), data }))
  }
}

/**
 * Register one operation as `parent <name...>`. Returns the command so a
 * caller can add an alias or extra help.
 */
export function endpoint<F extends (options: never) => unknown>(
  parent: Command,
  fn: F,
  spec: EndpointSpec<OptionsOf<F>>,
): Command {
  const words = spec.name.trim().split(/\s+/)
  const leaf = words.pop() as string
  let group = parent
  for (const word of words) group = subgroup(group, word, `${humanize(word)} commands`)
  if (group.commands.some((c) => c.name() === leaf)) {
    throw new Error(`"${[parent.name(), ...words, leaf].join(' ')}" is registered twice`)
  }

  const pathEntries = Object.entries((spec.path ?? {}) as Record<string, 'int' | 'string' | 'project'>)
  const positional = pathEntries.filter(([, kind]) => kind !== 'project')
  const projectParam = pathEntries.find(([, kind]) => kind === 'project')?.[0]
  const cmd = group.command([leaf, ...positional.map(([name]) => argName(name))].join(' ')).description(spec.description)

  const taken = new Set<string>()
  if (projectParam) cmd.option('-p, --project <slug-or-id>', 'Project slug or ID (default: the linked project)')

  const query = normalizeFields(spec.query as Record<string, unknown> | undefined)
  const bodyMode = typeof spec.body === 'string' ? (spec.body as '@json' | '@raw') : spec.body ? 'fields' : undefined
  const body = bodyMode === 'fields' ? normalizeFields(spec.body as unknown as Record<string, unknown>) : []
  const queryTaken = new Set(taken)
  addFieldOptions(cmd, query, 'query', queryTaken)
  addFieldOptions(cmd, body, 'body', new Set([...queryTaken]))

  if (bodyMode === 'fields') {
    cmd.option('--from-file <path>', 'Start the request body from a JSON file (- for stdin); flags override it')
    cmd.option('--set <field=value>', 'Set a body field by path, e.g. limits.memory=512 (repeatable)', collect)
  }
  if (bodyMode === '@json') cmd.option('--body <json>', 'The request body as JSON (or --from-file)').option('--from-file <path>', 'Read the request body from a JSON file (- for stdin)')
  if (bodyMode === '@raw') cmd.requiredOption('--input <path>', 'The file to upload (- for stdin)')
  if (spec.output === 'bytes') cmd.option('-o, --output <path>', 'Where to write the reply (default: stdout)', '-')
  if (spec.confirm) cmd.option('-y, --yes', 'Do not ask for confirmation')
  cmd.option('--json', spec.output === 'events' ? 'Accepted for symmetry: events are always JSON lines' : 'Output in JSON format')

  cmd.action(async (...args: unknown[]) => {
    const options = (args[positional.length] ?? {}) as Record<string, unknown>
    const path: Record<string, unknown> = {}
    positional.forEach(([name, kind], index) => {
      const raw = String(args[index])
      path[name] = kind === 'int' ? parseIntegerFlag(raw, argName(name), 0) : raw
    })

    const queryValues: Record<string, unknown> = {}
    for (const field of query) {
      const value = await fieldValue(field, options)
      if (value !== undefined) queryValues[field.name] = value
      else if (field.required) throw new ValidationError(`${field.option?.long ?? field.name} is required`)
    }

    if (spec.output === 'terminal') {
      const size = terminalSize()
      if (query.some((f) => f.name === 'cols') && queryValues.cols === undefined) queryValues.cols = size.cols
      if (query.some((f) => f.name === 'rows') && queryValues.rows === undefined) queryValues.rows = size.rows
    }

    let requestBody: unknown
    if (bodyMode === 'fields') {
      let document: JsonObject = options.fromFile ? await readJsonObject(String(options.fromFile), 'The request body') : {}
      for (const field of body) {
        const value = await fieldValue(field, options)
        if (value !== undefined) document[field.name] = value
      }
      const assignments = ((options.set as string[] | undefined) ?? []).map(parseAssignment)
      document = applyAssignments(document, assignments, { allowNew: true })
      const missing = body.filter((field) => field.required && document[field.name] === undefined)
      if (missing.length) {
        const flags = missing.map((field) => field.option?.long ?? field.stdinOption?.long ?? field.name)
        throw new ValidationError(`Missing required ${flags.length === 1 ? 'input' : 'inputs'}: ${flags.join(', ')}`)
      }
      requestBody = document
    } else if (bodyMode === '@json') {
      if (options.body !== undefined && options.fromFile !== undefined) throw new ValidationError('Pass --body or --from-file, not both')
      if (options.body !== undefined) requestBody = parseJsonFlag(String(options.body), '--body')
      else if (options.fromFile !== undefined) requestBody = await readJsonObject(String(options.fromFile), 'The request body')
      else throw new ValidationError('Pass the request body with --body <json> or --from-file <path>')
    } else if (bodyMode === '@raw') {
      const input = String(options.input)
      const source = input === '-' ? Bun.stdin : Bun.file(input)
      if (input !== '-' && !(await source.exists())) throw new ValidationError(`--input: file not found: ${input}`)
      requestBody = source
    }
    if (spec.confirm && !(await confirmRisk([spec.confirm], 'Continue?', Boolean(options.yes)))) return

    await connect()
    if (projectParam) path[projectParam] = (await resolveProjectFlag(options.project as string | undefined)).id

    const request: Record<string, unknown> = { client }
    if (Object.keys(path).length) request.path = path
    if (Object.keys(queryValues).length) request.query = queryValues
    if (requestBody !== undefined) request.body = requestBody
    const what = `Could not ${spec.description.charAt(0).toLowerCase()}${spec.description.slice(1)}`
    const invoke = fn as unknown as (options: Record<string, unknown>) => Promise<unknown>

    if (spec.output === 'socket' || spec.output === 'terminal') {
      // The SDK builds the URL (path and query serialization); capture it
      // instead of sending the request, then open the socket ourselves.
      let url: string | undefined
      await invoke({
        ...request,
        fetch: async (input: Request | string | URL) => {
          url = input instanceof Request ? input.url : String(input)
          return new Response(null, { status: 204 })
        },
      })
      if (!url) throw new Error(`${what}: could not build the WebSocket URL`)
      const wsUrl = url.replace(/^http(s?):/, 'ws$1:')
      if (spec.output === 'terminal') await attachTerminal(wsUrl, spec.name)
      else await streamSocket(wsUrl, what)
      return
    }

    if (spec.output === 'events') {
      const meta: { event?: string; id?: string }[] = []
      let failure: unknown
      const result = (await invoke({
        ...request,
        sseMaxRetryAttempts: 1,
        onSseEvent: (event: { event?: string; id?: string }) => meta.push({ event: event.event, id: event.id }),
        onSseError: (error: unknown) => {
          failure = error
        },
      })) as { stream: AsyncGenerator<unknown> }
      const quiet = isQuietMode()
      setQuietMode(true)
      try {
        await printEvents(result.stream, meta)
      } finally {
        setQuietMode(quiet)
      }
      if (failure) throw new Error(`${what}: ${failure instanceof Error ? failure.message : String(failure)}`)
      return
    }

    if (spec.output === 'bytes') {
      const { data, error, response } = (await invoke({ ...request, parseAs: 'stream' })) as {
        data?: unknown
        error?: unknown
        response?: Response
      }
      if (error !== undefined || !response?.ok) {
        await call(what, async () => ({ error: error ?? `HTTP ${response?.status}`, response }))
      }
      const reply = new Response((data ?? null) as ReadableStream | null)
      const target = String(options.output ?? '-')
      if (target === '-') {
        await Bun.write(Bun.stdout, reply)
        return
      }
      const written = await Bun.write(target, reply)
      if (!options.json) success(`Saved ${written} bytes to ${target}`)
      else output({ path: target, bytes: written }, { json: true })
      return
    }

    const data = await call(what, () => invoke(request) as Promise<{ data?: unknown; error?: unknown; response?: Response }>)
    const items = listItems(data)
    if (!options.json && items) {
      printList(items, spec.description, 'Nothing to show')
      return
    }
    output(data, { json: Boolean(options.json) }, spec.description, data === undefined ? 'Done' : undefined)
  })
  return cmd
}
