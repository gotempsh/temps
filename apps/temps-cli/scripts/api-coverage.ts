// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Everything the Temps HTTP API can do must be doable from the CLI.
 *
 * This check reads the committed spec (`openapi.json`) and the command
 * sources under `src/commands/`, and reports three kinds of gap:
 *
 * - `METHOD /path`: no command calls the operation, neither through its
 *   generated SDK function nor through a direct `client.<method>({ url })`.
 * - `METHOD /path body.<field>` / `query.<field>`: a command calls the
 *   operation but never names that request-body property or query parameter,
 *   so no flag can set it.
 * - `METHOD /path --json`: a read is called, but the command area offers no
 *   `--json` output, so its response cannot be printed in full.
 *
 * A command area is a directory directly under `src/commands/` (or a
 * top-level command file). A field counts as settable when its name appears
 * in an area that calls the operation; that is a static approximation, cheap
 * enough to run on every change, that catches the common gap (a flag nobody
 * added) without annotating every command.
 *
 * `cli-api-coverage.json` lists what is not covered:
 *
 * - `excluded`: gaps intentionally left out of the CLI, grouped by a reviewed
 *   reason (browser-only flows, SDK ingest endpoints, callbacks).
 *
 * Any other gap fails: a new route ships with its command, or with an
 * `excluded` entry and the reason it has none. There is no backlog list:
 * the `pending` list that held the gaps found when the check was added is
 * empty and closed, and an entry in it fails the check.
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

/**
 * The filesystem directory above the module at `moduleUrl`. `fileURLToPath`
 * decodes the URL, so a checkout under `my dir` or `50%` resolves to the
 * real path rather than `my%20dir` or `50%25`.
 */
export function parentDirOf(moduleUrl: string | URL): string {
  return fileURLToPath(new URL('..', moduleUrl))
}

export const CLI_ROOT = parentDirOf(import.meta.url)
export const ALLOWLIST_PATH = join(CLI_ROOT, 'cli-api-coverage.json')
export const SPEC_FILE = join(CLI_ROOT, 'openapi.json')
export const SDK_FILE = join(CLI_ROOT, 'src/api/sdk.gen.ts')
export const COMMANDS_DIR = join(CLI_ROOT, 'src/commands')

const METHODS = ['get', 'put', 'post', 'delete', 'patch', 'head'] as const
type Method = (typeof METHODS)[number]

export interface Operation {
  /** `METHOD /path`, the key used in reports and in the allowlist. */
  key: string
  method: Method
  path: string
  operationId: string
  /** `body.<name>` and `query.<name>` inputs a command must be able to set. */
  fields: string[]
}

export interface ExclusionGroup {
  reason: string
  entries: string[]
}

export interface Allowlist {
  excluded: ExclusionGroup[]
  /** The closed backlog: must stay empty; an entry here fails the check. */
  pending?: string[]
}

/** Source text of each command area, keyed by its path relative to `src/commands`. */
export type CommandAreas = Map<string, string>

type Schema = Record<string, unknown>

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {}
}

/**
 * Top-level property names of a schema, following `$ref`, `allOf`, `oneOf`
 * and `anyOf` (every variant's properties count: a CLI must be able to send
 * any of them). `readOnly` properties are skipped.
 */
export function schemaProperties(schema: unknown, components: Record<string, unknown>, seen: string[] = []): string[] {
  const node = asRecord(schema)
  const ref = node['$ref']
  if (typeof ref === 'string') {
    const name = ref.split('/').pop() ?? ''
    if (seen.includes(name)) return []
    return schemaProperties(components[name], components, [...seen, name])
  }
  const names = new Set<string>()
  for (const key of ['allOf', 'oneOf', 'anyOf']) {
    const variants = node[key]
    if (Array.isArray(variants)) {
      for (const variant of variants) for (const name of schemaProperties(variant, components, seen)) names.add(name)
    }
  }
  for (const [name, property] of Object.entries(asRecord(node['properties']))) {
    if (asRecord(property)['readOnly'] === true) continue
    names.add(name)
  }
  return [...names]
}

/** Every operation in the spec with the inputs a command must expose. */
export function listOperations(spec: unknown): Operation[] {
  const root = asRecord(spec)
  const components = asRecord(asRecord(root['components'])['schemas'])
  const operations: Operation[] = []
  for (const [path, item] of Object.entries(asRecord(root['paths']))) {
    const pathItem = asRecord(item)
    const shared = Array.isArray(pathItem['parameters']) ? pathItem['parameters'] : []
    for (const method of METHODS) {
      const operation = pathItem[method]
      if (!operation) continue
      const op = asRecord(operation)
      const fields = new Set<string>()
      const parameters = [...shared, ...(Array.isArray(op['parameters']) ? op['parameters'] : [])]
      for (const parameter of parameters) {
        const p = asRecord(parameter)
        if (p['in'] === 'query' && typeof p['name'] === 'string') fields.add(`query.${p['name']}`)
      }
      for (const media of Object.values(asRecord(asRecord(op['requestBody'])['content']))) {
        for (const name of schemaProperties(asRecord(media)['schema'], components)) fields.add(`body.${name}`)
      }
      operations.push({
        key: `${method.toUpperCase()} ${path}`,
        method,
        path,
        operationId: String(op['operationId'] ?? ''),
        fields: [...fields].sort(),
      })
    }
  }
  return operations.sort((a, b) => a.key.localeCompare(b.key))
}

/** `{param}` and `${expr}` segments compare equal, and a leading `/` is optional. */
export function normalizePath(path: string): string {
  return ('/' + path.replace(/^\/+/, '')).replace(/\$\{[^}]*\}/g, '{}').replace(/\{[^}]*\}/g, '{}')
}

/**
 * Generated SDK function names for each `METHOD /path`, read from
 * `sdk.gen.ts` so the mapping follows the generator, not a naming guess.
 */
export function sdkFunctionsByOperation(sdkSource: string): Map<string, string[]> {
  const functions = new Map<string, string[]>()
  const exports = [...sdkSource.matchAll(/^export const (\w+) = /gm)]
  exports.forEach((match, index) => {
    const body = sdkSource.slice(match.index, exports[index + 1]?.index ?? sdkSource.length)
    const call = body.match(/\)\.(?:sse\.)?(get|put|post|delete|patch|head)</)
    const url = body.match(/url: '([^']+)'/)
    if (!call?.[1] || !url?.[1]) return
    const key = `${call[1].toUpperCase()} ${normalizePath(url[1])}`
    functions.set(key, [...(functions.get(key) ?? []), match[1] as string])
  })
  return functions
}

/** `METHOD /normalized-path` of every direct `client.<method>({ url })` call in a source. */
export function directClientCalls(source: string): Set<string> {
  const calls = new Set<string>()
  const pattern = /client\.(get|put|post|delete|patch|head)\s*(?:<[^(]*>)?\(\s*\{[\s\S]{0,400}?url:\s*(['"`])(.*?)\2/g
  for (const match of source.matchAll(pattern)) {
    calls.add(`${(match[1] as string).toUpperCase()} ${normalizePath(match[3] as string)}`)
  }
  return calls
}

/**
 * `METHOD /normalized-path` of calls made without the generated client:
 *
 * - an explicit `// api-coverage: METHOD /path` marker, for helpers that
 *   compose the URL from a prefix and a relative path (the sandbox commands);
 * - in a file that calls `fetch(`, a string or template literal whose path,
 *   after an optional `${base}` prefix and without its query string, is a
 *   spec path. The method is a `method: '...'` that follows it in the same
 *   statement, or GET when the literal is `fetch`'s first argument; a
 *   literal with neither is not counted, its method being unknown.
 */
const MARKER = /api-coverage:\s*(GET|PUT|POST|DELETE|PATCH|HEAD)\s+(\/\S*)/g

export function rawFetchCalls(source: string, specPaths: ReadonlySet<string>): Set<string> {
  const calls = new Set<string>()
  for (const match of source.matchAll(MARKER)) {
    calls.add(`${match[1]} ${normalizePath(match[2] as string)}`)
  }
  if (!/\bfetch\(/.test(source)) return calls
  for (const match of source.matchAll(/(['"`])((?:\$\{[^}]*\})?\/[^'"`\s]*)\1/g)) {
    const literal = (match[2] as string).replace(/^\$\{[^}]*\}/, '').split('?')[0] as string
    // A trailing `${...}` is a path parameter (`/widgets/${id}`) or a query
    // string appended to the path (`/widgets${qs}`): try both readings.
    const path = [normalizePath(literal), normalizePath(literal.replace(/\$\{[^}]*\}$/, ''))].find((p) => specPaths.has(p))
    if (!path) continue
    const start = match.index ?? 0
    // The rest of this call: up to the end of the statement or the next request.
    const after = source.slice(start + match[0].length, start + match[0].length + 300).split(/;|\bfetch\(|\n\s*\n/)[0] ?? ''
    const method =
      after.match(/method:\s*['"](GET|PUT|POST|DELETE|PATCH|HEAD)['"]/)?.[1] ??
      (/fetch\(\s*$/.test(source.slice(Math.max(0, start - 20), start)) ? 'GET' : undefined)
    if (method) calls.add(`${method} ${path}`)
  }
  return calls
}

/** Group the non-test sources under `src/commands` into command areas. */
export function readCommandAreas(dir: string = COMMANDS_DIR): CommandAreas {
  const areas: CommandAreas = new Map()
  const walk = (current: string) => {
    for (const entry of readdirSync(current).sort()) {
      const full = join(current, entry)
      if (statSync(full).isDirectory()) {
        walk(full)
        continue
      }
      if (!/\.tsx?$/.test(entry) || /\.test\.tsx?$/.test(entry)) continue
      const area = relative(dir, full).split('/')[0] as string
      areas.set(area, (areas.get(area) ?? '') + '\n' + readFileSync(full, 'utf8'))
    }
  }
  walk(dir)
  return areas
}

const ENDPOINT_IMPORT = /from '(?:\.\.\/)+lib\/endpoint\.js'/

/** Whether `identifier` appears as a whole word; every regex character in it is literal. */
export function mentions(source: string, identifier: string): boolean {
  return new RegExp(`(?<![\\w$])${identifier.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w$])`).test(source)
}

/**
 * `source` with every comment blanked out (newlines kept), leaving strings
 * and template literals intact, so `'https://x'` keeps its `//` while
 * `const a = 1 // listWidgets` loses the comment. Regex literals are not
 * parsed; one containing `//` unescaped would be cut, which this codebase
 * does not have (a `/` in a regex is written `\/`).
 */
export function stripComments(source: string): string {
  let out = ''
  let i = 0
  // Open template literals; each entry counts the `{` braces open inside its current `${...}`.
  const templates: number[] = []
  let quote: "'" | '"' | null = null
  let inTemplate = false
  while (i < source.length) {
    const c = source[i] as string
    const next = source[i + 1]
    if (quote) {
      out += c
      if (c === '\\') {
        out += next ?? ''
        i += 2
        continue
      }
      if (c === quote || c === '\n') quote = null
      i++
      continue
    }
    if (inTemplate) {
      out += c
      if (c === '\\') {
        out += next ?? ''
        i += 2
        continue
      }
      if (c === '`') inTemplate = false
      else if (c === '$' && next === '{') {
        out += '{'
        templates.push(0)
        inTemplate = false
        i += 2
        continue
      }
      i++
      continue
    }
    if (c === '/' && next === '/') {
      while (i < source.length && source[i] !== '\n') i++
      continue
    }
    if (c === '/' && next === '*') {
      const end = source.indexOf('*/', i + 2)
      const stop = end === -1 ? source.length : end + 2
      out += source.slice(i, stop).replace(/[^\n]/g, ' ')
      i = stop
      continue
    }
    if (c === "'" || c === '"') quote = c
    else if (c === '`') inTemplate = true
    else if (templates.length) {
      const depth = templates.length - 1
      const open = templates[depth] as number
      if (c === '{') templates[depth] = open + 1
      else if (c === '}') {
        if (open === 0) {
          templates.pop()
          inTemplate = true
        } else templates[depth] = open - 1
      }
    }
    out += c
    i++
  }
  return out
}

/**
 * The code of an area without its import declarations and comments, so an
 * SDK function that is imported or named in a comment but never used does
 * not count as called. Imports of the generated SDK under another name
 * (`listWidgets as fetchWidgets`) are returned so the local name counts.
 */
export function codeOf(source: string): { code: string; aliases: Map<string, string[]>; namespaces: string[] } {
  const aliases = new Map<string, string[]>()
  const namespaces: string[] = []
  const code = stripComments(source).replace(
    /^\s*import\s[\s\S]*?\sfrom\s+['"]([^'"]+)['"];?[ \t]*$/gm,
    (statement, from: string) => {
      if (/api\/sdk\.gen(\.js)?$/.test(from)) {
        // `import * as api from '.../sdk.gen.js'`: calls are `api.name(...)`.
        const namespace = /^\s*import\s+\*\s+as\s+(\w+)/.exec(statement)
        if (namespace) {
          namespaces.push(namespace[1] as string)
          return ''
        }
        for (const m of statement.matchAll(/(\w+)\s+as\s+(\w+)/g)) {
          aliases.set(m[1] as string, [...(aliases.get(m[1] as string) ?? []), m[2] as string])
        }
      }
      return ''
    },
  )
  return { code, aliases, namespaces }
}

/**
 * Whether `code` calls the SDK function `name` (`name(` or `name<T>(`, or
 * `ns.name(` through a namespace import, never another object's `x.name(`)
 * or registers it as a command (`endpoint(group, name, ...)`).
 * A bare reference (`const f = name`, `{ list: name }`) is not a call.
 */
export function callsFunction(code: string, name: string, namespaces: readonly string[] = []): boolean {
  const id = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const qualified = namespaces.map((ns) => `(?<![\\w$.])${ns}\\s*\\.\\s*${id}\\s*(?:<[^>()]*>\\s*)?\\(`)
  return (
    new RegExp(`(?<![\\w$.])${id}\\s*(?:<[^>()]*>\\s*)?\\(`).test(code) ||
    qualified.some((pattern) => new RegExp(pattern).test(code)) ||
    new RegExp(`\\bendpoint\\(\\s*[\\w$.]+\\s*,\\s*${id}\\s*[,)]`).test(code)
  )
}

export interface CoverageReport {
  operations: Operation[]
  /** Operations some command calls. */
  covered: string[]
  /** Every gap, sorted: operations, then `<op> body.x`/`query.x`, then `<op> --json`. */
  gaps: string[]
  /** Fields that are settable, out of the fields of covered operations. */
  fieldsCovered: number
  fieldsTotal: number
  /** `api-coverage:` markers that name no operation in the spec (a typo, or a removed route). */
  unknownMarkers: string[]
}

/** Coverage of the spec by the command areas. */
export function computeCoverage(spec: unknown, sdkSource: string, areas: CommandAreas): CoverageReport {
  const operations = listOperations(spec)
  const sdk = sdkFunctionsByOperation(sdkSource)
  const specPaths = new Set(operations.map((op) => normalizePath(op.path)))
  const code = new Map([...areas].map(([area, source]) => [area, codeOf(source)]))
  const direct = new Map(
    [...areas].map(([area, source]) => [
      area,
      new Set([...directClientCalls(source), ...rawFetchCalls(source, specPaths)]),
    ]),
  )
  const covered: string[] = []
  const gaps: string[] = []
  let fieldsCovered = 0
  let fieldsTotal = 0
  for (const op of operations) {
    const normalized = `${op.method.toUpperCase()} ${normalizePath(op.path)}`
    const names = sdk.get(normalized) ?? []
    const callers = [...areas].filter(([area]) => {
      const { code: text, aliases, namespaces } = code.get(area) as ReturnType<typeof codeOf>
      return (
        direct.get(area)?.has(normalized) ||
        names.some((name) => [name, ...(aliases.get(name) ?? [])].some((local) => callsFunction(text, local, namespaces)))
      )
    })
    if (!callers.length) {
      gaps.push(op.key)
      continue
    }
    covered.push(op.key)
    const text = callers.map(([area]) => code.get(area)?.code ?? '').join('\n')
    for (const field of op.fields) {
      fieldsTotal++
      const name = field.slice(field.indexOf('.') + 1)
      if (mentions(text, name)) fieldsCovered++
      else gaps.push(`${op.key} ${field}`)
    }
    // `endpoint()` (src/lib/endpoint.ts) gives every command it registers `--json`.
    const raw = callers.map(([, source]) => source).join('\n')
    if (op.method === 'get' && !text.includes('--json') && !ENDPOINT_IMPORT.test(raw)) gaps.push(`${op.key} --json`)
  }
  const known = new Set(operations.map((op) => `${op.method.toUpperCase()} ${normalizePath(op.path)}`))
  const unknownMarkers = new Set<string>()
  for (const [area, source] of areas) {
    for (const match of source.matchAll(MARKER)) {
      const marker = `${match[1]} ${match[2]}`
      if (!known.has(`${match[1]} ${normalizePath(match[2] as string)}`)) unknownMarkers.add(`${area}: ${marker}`)
    }
  }
  return { operations, covered, gaps: gaps.sort(), fieldsCovered, fieldsTotal, unknownMarkers: [...unknownMarkers].sort() }
}

/** The operation an allowlist entry or gap refers to (`METHOD /path`). */
export function operationOf(entry: string): string {
  const [method, path] = entry.split(' ')
  return `${method} ${path}`
}

export interface CoverageDrift {
  /** Gaps that are not excluded. */
  uncovered: string[]
  /** Entries in the closed `pending` list. */
  pending: string[]
  /** `excluded` entries that are covered now or whose operation is gone. */
  staleExcluded: string[]
  /** Exclusion groups without a reason. */
  unexplained: number
  /** Entries listed more than once across both lists. */
  duplicates: string[]
  /** `api-coverage:` markers that name no operation. */
  unknownMarkers: string[]
}

export function coverageDrift(report: CoverageReport, allowlist: Allowlist): CoverageDrift {
  const gaps = new Set(report.gaps)
  const excluded = allowlist.excluded.flatMap((group) => group.entries)
  const listed = excluded
  const seen = new Set<string>()
  const duplicates = new Set<string>()
  for (const entry of listed) {
    if (seen.has(entry)) duplicates.add(entry)
    seen.add(entry)
  }
  // A whole-operation exclusion also covers the field gaps it would have once called.
  const excludedOps = new Set(excluded.filter((entry) => entry === operationOf(entry)))
  return {
    uncovered: report.gaps.filter((gap) => !seen.has(gap) && !excludedOps.has(operationOf(gap))),
    pending: [...(allowlist.pending ?? [])].sort(),
    staleExcluded: excluded.filter((entry) => !gaps.has(entry)).sort(),
    unexplained: allowlist.excluded.filter((group) => !group.reason.trim()).length,
    duplicates: [...duplicates].sort(),
    unknownMarkers: report.unknownMarkers,
  }
}

export function describeCoverageDrift(drift: CoverageDrift): string | null {
  const lines: string[] = []
  const section = (title: string, entries: string[], hint: string) => {
    if (!entries.length) return
    if (lines.length) lines.push('')
    lines.push(title, ...entries.map((entry) => `  ${entry}`), hint)
  }
  section(
    'These API operations or inputs have no CLI command or flag:',
    drift.uncovered,
    'Add the command or flag, or, if the CLI should not expose it, add it to an `excluded` group in ' +
      'apps/temps-cli/cli-api-coverage.json with the reason.',
  )
  section(
    'apps/temps-cli/cli-api-coverage.json lists gaps in `pending`, a closed list:',
    drift.pending,
    'Add the command or flag, or move the entry to an `excluded` group with the reason; then remove `pending`.',
  )
  section(
    'These `excluded` entries in apps/temps-cli/cli-api-coverage.json are covered now or no longer exist; remove them:',
    drift.staleExcluded,
    '',
  )
  section('These entries are listed more than once:', drift.duplicates, '')
  section(
    'These `// api-coverage: METHOD /path` markers name no operation in openapi.json:',
    drift.unknownMarkers,
    'Fix the method or path, or remove the marker if the route is gone.',
  )
  if (drift.unexplained) {
    if (lines.length) lines.push('')
    lines.push(`${drift.unexplained} exclusion group(s) have no reason; every exclusion says why.`)
  }
  return lines.length ? lines.join('\n').trimEnd() : null
}

export function readAllowlist(path: string = ALLOWLIST_PATH): Allowlist {
  const raw = asRecord(JSON.parse(readFileSync(path, 'utf8')))
  const excluded = Array.isArray(raw['excluded']) ? raw['excluded'] : []
  return {
    excluded: excluded.map((group) => {
      const g = asRecord(group)
      return {
        reason: String(g['reason'] ?? ''),
        entries: Array.isArray(g['entries']) ? g['entries'].map(String) : [],
      }
    }),
    ...(Array.isArray(raw['pending']) && raw['pending'].length > 0 && { pending: raw['pending'].map(String) }),
  }
}

export function formatAllowlist(allowlist: Allowlist): string {
  const sorted: Allowlist = {
    excluded: allowlist.excluded.map((group) => ({ reason: group.reason, entries: [...group.entries].sort() })),
    ...(allowlist.pending?.length && { pending: [...allowlist.pending].sort() }),
  }
  return JSON.stringify(sorted, null, 2) + '\n'
}

/** The coverage of the committed spec by the committed commands. */
export function repositoryCoverage(): CoverageReport {
  const spec = JSON.parse(readFileSync(SPEC_FILE, 'utf8')) as Schema
  return computeCoverage(spec, readFileSync(SDK_FILE, 'utf8'), readCommandAreas())
}

export function summarize(report: CoverageReport, allowlist: Allowlist): string {
  const excludedOps = new Set(
    allowlist.excluded.flatMap((group) => group.entries).filter((entry) => entry === operationOf(entry)),
  )
  const total = report.operations.length
  const excludedInputs = allowlist.excluded.flatMap((group) => group.entries).filter((entry) => entry !== operationOf(entry))
  const percent = (n: number, d: number) => (d ? ((100 * n) / d).toFixed(1) : '100.0')
  return [
    `Operations: ${report.covered.length}/${total} called by a command (${percent(report.covered.length, total)}%), ` +
      `${excludedOps.size} excluded`,
    `Inputs of called operations: ${report.fieldsCovered}/${report.fieldsTotal} settable ` +
      `(${percent(report.fieldsCovered, report.fieldsTotal)}%), ${excludedInputs.length} excluded`,
  ].join('\n')
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const report = repositoryCoverage()
  const allowlist = readAllowlist()
  if (args.includes('--list')) {
    for (const gap of report.gaps) console.log(gap)
    process.exit(0)
  }
  console.log(summarize(report, allowlist))
  const failure = describeCoverageDrift(coverageDrift(report, allowlist))
  if (failure) {
    console.error('\n' + failure)
    process.exit(1)
  }
}
