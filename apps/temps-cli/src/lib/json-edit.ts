// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Editing a nested request body from the command line, for endpoints whose
 * body is too deep for one flag per field (instance settings, CI builder,
 * policies): `path.to.field=value` assignments applied to the current
 * document, and a whole document read from a JSON file or stdin.
 *
 * Values are JSON when they parse as JSON (`true`, `30`, `["a"]`, `null`)
 * and strings otherwise, so `external_url=https://x.example` needs no quotes;
 * `path:=value` forces a string (`version:=1.10` keeps the text).
 */

import { ValidationError } from '../utils/errors.js'

/** Largest JSON document read from a file or stdin. Request bodies are capped well below this. */
export const MAX_JSON_INPUT_BYTES = 1_048_576

export type JsonObject = Record<string, unknown>

export function isJsonObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export interface Assignment {
  path: string[]
  value: unknown
}

/** A command-line value: JSON when it parses, else the text itself. */
export function parseCliValue(raw: string): unknown {
  const trimmed = raw.trim()
  if (trimmed === '') return raw
  try {
    return JSON.parse(trimmed)
  } catch {
    return raw
  }
}

/** Split a dotted path, refusing empty segments. */
export function parsePath(raw: string): string[] {
  const path = raw.trim().split('.')
  if (path.some((segment) => segment.trim() === '')) {
    throw new ValidationError(`"${raw}" is not a field path; use dotted names such as shield.enabled`)
  }
  return path.map((segment) => segment.trim())
}

/** Parse `path=value` (JSON or text) or `path:=value` (always text). */
export function parseAssignment(raw: string): Assignment {
  const index = raw.indexOf('=')
  if (index <= 0) {
    throw new ValidationError(`"${raw}" is not an assignment; write field=value, e.g. shield.enabled=true`)
  }
  const forceString = raw[index - 1] === ':'
  const path = parsePath(raw.slice(0, forceString ? index - 1 : index))
  const text = raw.slice(index + 1)
  return { path, value: forceString ? text : parseCliValue(text) }
}

export function getAtPath(document: unknown, path: string[]): unknown {
  let current: unknown = document
  for (const segment of path) {
    if (!isJsonObject(current)) return undefined
    current = current[segment]
  }
  return current
}

export interface SetOptions {
  /** Allow a field the current document does not have (and create missing parents). */
  allowNew?: boolean
}

/**
 * A copy of `document` with `value` at `path`. Without `allowNew`, every
 * segment must already exist, so a typo (`shield.enabled`) is refused
 * instead of being sent and silently ignored by the server.
 */
export function setAtPath(document: JsonObject, path: string[], value: unknown, options: SetOptions = {}): JsonObject {
  const [head, ...rest] = path
  if (head === undefined) throw new ValidationError('An empty field path cannot be set')
  if (!options.allowNew && !(head in document)) {
    const known = Object.keys(document).sort()
    throw new ValidationError(
      `Unknown field "${head}". Known fields here: ${known.length ? known.join(', ') : '(none)'}`,
    )
  }
  if (rest.length === 0) return { ...document, [head]: value }
  const child = document[head]
  if (child === undefined || child === null) {
    if (!options.allowNew && child === undefined) throw new ValidationError(`Unknown field "${head}"`)
    // A section that is unset (null) is created from the assignment.
    return { ...document, [head]: setAtPath({}, rest, value, { allowNew: true }) }
  }
  if (!isJsonObject(child)) {
    throw new ValidationError(`"${head}" is a ${Array.isArray(child) ? 'list' : typeof child}, not a section with fields`)
  }
  return { ...document, [head]: setAtPath(child, rest, value, options) }
}

export function applyAssignments(document: JsonObject, assignments: Assignment[], options: SetOptions = {}): JsonObject {
  return assignments.reduce((current, a) => setAtPath(current, a.path, a.value, options), document)
}

/** Text from `chunks`, refusing (with `tooLarge()`) once it passes MAX_JSON_INPUT_BYTES, before buffering the rest. */
async function readBounded(chunks: AsyncIterable<Uint8Array | string>, tooLarge: () => string): Promise<string> {
  const buffers: Buffer[] = []
  let size = 0
  for await (const chunk of chunks) {
    const buffer = Buffer.from(chunk)
    size += buffer.length
    if (size > MAX_JSON_INPUT_BYTES) throw new ValidationError(tooLarge())
    buffers.push(buffer)
  }
  return Buffer.concat(buffers).toString('utf8')
}

/**
 * A file's text, refusing one over MAX_JSON_INPUT_BYTES without reading it:
 * a regular file by its size, a pipe or device (no size) by streaming.
 */
async function readFileBounded(source: string, what: string): Promise<string> {
  const tooLarge = () => `${what} in ${source} is larger than ${MAX_JSON_INPUT_BYTES} bytes`
  const file = Bun.file(source)
  try {
    if (file.size > MAX_JSON_INPUT_BYTES) throw new ValidationError(tooLarge())
    // Bun's ReadableStream is async-iterable at runtime; the DOM typings omit it.
    return await readBounded(file.stream() as unknown as AsyncIterable<Uint8Array>, tooLarge)
  } catch (err) {
    if (err instanceof ValidationError) throw err
    throw new ValidationError(`Could not read ${what} from ${source}: ${(err as Error).message}`)
  }
}

/** Read a JSON object from a file path, or stdin for `-`. */
export async function readJsonObject(
  source: string,
  what: string,
  stdin: AsyncIterable<Uint8Array | string> = process.stdin,
): Promise<JsonObject> {
  const text =
    source === '-'
      ? await readBounded(stdin, () => `${what} on stdin is larger than ${MAX_JSON_INPUT_BYTES} bytes`)
      : await readFileBounded(source, what)
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch (err) {
    throw new ValidationError(`${what} in ${source === '-' ? 'stdin' : source} is not JSON: ${(err as Error).message}`)
  }
  if (!isJsonObject(parsed)) throw new ValidationError(`${what} must be a JSON object`)
  return parsed
}

/**
 * Refuse fields a typed request does not have. `known` is a table keyed by
 * every field of the request type (`satisfies Record<keyof T, string>`), so
 * a field added to the API fails the typecheck until the CLI knows it.
 */
export function rejectUnknownFields(document: JsonObject, known: Record<string, string>, what: string): void {
  const unknown = Object.keys(document).filter((key) => !(key in known))
  if (unknown.length) {
    throw new ValidationError(
      `${what} has unknown field(s) ${unknown.join(', ')}. Known: ${Object.keys(known).sort().join(', ')}`,
    )
  }
}

/** Commander collector for a repeatable option. */
export function collect(value: string, previous: string[] = []): string[] {
  return [...previous, value]
}
