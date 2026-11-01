// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EnvironmentResponse,
  EnvironmentVariableResponse,
} from '@/api/client/types.gen'

/**
 * `.env` text <-> variables for the raw editor.
 *
 * The editor shows an environment's variables as `.env` text, lets the user
 * edit it freely, and turns the edited text back into per-variable API calls.
 * Opening and saving without touching anything must therefore produce zero
 * changes: `parseEnvText(serializeEnvText(x))` returns `x` for every value,
 * including ones with newlines, quotes, backslashes, `#` and surrounding
 * whitespace.
 */

export interface EnvEntry {
  key: string
  value: string
}

export interface EnvParseError {
  /** 1-based line number in the edited text. */
  line: number
  message: string
}

export interface EnvParseResult {
  entries: EnvEntry[]
  errors: EnvParseError[]
}

const KEY_PATTERN = /^[^\s=#"'`]+$/

/** Characters that force double quotes so the value round-trips. */
function needsQuotes(value: string): boolean {
  return value !== value.trim() || /[\n\r\t"'`\\#]/.test(value)
}

function quote(value: string): string {
  const escaped = value
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\n/g, '\\n')
    .replace(/\r/g, '\\r')
    .replace(/\t/g, '\\t')
  return `"${escaped}"`
}

export function serializeEnvText(entries: readonly EnvEntry[]): string {
  if (entries.length === 0) return ''
  return (
    entries
      .map(({ key, value }) =>
        needsQuotes(value) ? `${key}=${quote(value)}` : `${key}=${value}`
      )
      .join('\n') + '\n'
  )
}

const DOUBLE_QUOTE_ESCAPES: Record<string, string> = {
  n: '\n',
  r: '\r',
  t: '\t',
  '"': '"',
  '\\': '\\',
}

/**
 * Reads a double-quoted value starting just after the opening quote.
 * Returns the decoded value and the text after the closing quote, or `null`
 * when the quote is never closed in `source`.
 */
function readDoubleQuoted(
  source: string
): { value: string; rest: string } | null {
  let value = ''
  for (let index = 0; index < source.length; index++) {
    const char = source[index]
    if (char === '\\' && index + 1 < source.length) {
      const next = source[index + 1]
      const decoded = DOUBLE_QUOTE_ESCAPES[next]
      value += decoded ?? `\\${next}`
      index++
      continue
    }
    if (char === '"') return { value, rest: source.slice(index + 1) }
    value += char
  }
  return null
}

function isOnlyComment(rest: string): boolean {
  const trimmed = rest.trim()
  return trimmed === '' || trimmed.startsWith('#')
}

/**
 * Parses `.env` text. Follows the common dotenv conventions so a file pasted
 * from elsewhere means what it meant there:
 * - blank lines and `#` comments are ignored, an `export ` prefix is allowed
 * - unquoted values are trimmed and end at ` #` (an inline comment)
 * - `'single'` quotes are literal, `"double"` quotes decode `\n \r \t \" \\`
 *   and may span several lines
 *
 * Unlike a loader, it reports every malformed line and duplicate key instead
 * of skipping or overwriting: the result replaces stored variables, so a
 * silently dropped line would delete one.
 */
export function parseEnvText(text: string): EnvParseResult {
  const lines = text.replace(/\r\n/g, '\n').split('\n')
  const entries: EnvEntry[] = []
  const errors: EnvParseError[] = []
  const firstLineByKey = new Map<string, number>()

  for (let index = 0; index < lines.length; index++) {
    const lineNumber = index + 1
    const trimmed = lines[index].trim()
    if (!trimmed || trimmed.startsWith('#')) continue

    const assignment = trimmed.replace(/^export\s+/, '')
    const equals = assignment.indexOf('=')
    if (equals < 0) {
      errors.push({
        line: lineNumber,
        message: 'Expected KEY=value',
      })
      continue
    }
    const key = assignment.slice(0, equals).trim()
    if (!KEY_PATTERN.test(key)) {
      errors.push({
        line: lineNumber,
        message: key
          ? `"${key}" is not a valid variable name`
          : 'Variable name is missing before "="',
      })
      continue
    }

    const raw = assignment.slice(equals + 1).trimStart()
    let value: string

    if (raw.startsWith('"')) {
      // A double-quoted value may continue on the following lines.
      let source = raw.slice(1)
      let parsed = readDoubleQuoted(source)
      let lastLine = index
      while (!parsed && lastLine + 1 < lines.length) {
        lastLine++
        source += `\n${lines[lastLine]}`
        parsed = readDoubleQuoted(source)
      }
      if (!parsed) {
        errors.push({
          line: lineNumber,
          message: `Value of ${key} opens a " that is never closed`,
        })
        break
      }
      if (!isOnlyComment(parsed.rest)) {
        errors.push({
          line: lastLine + 1,
          message: `Unexpected text after the closing " of ${key}`,
        })
      }
      index = lastLine
      value = parsed.value
    } else if (raw.startsWith("'")) {
      const closing = raw.indexOf("'", 1)
      if (closing < 0) {
        errors.push({
          line: lineNumber,
          message: `Value of ${key} opens a ' that is never closed`,
        })
        continue
      }
      if (!isOnlyComment(raw.slice(closing + 1))) {
        errors.push({
          line: lineNumber,
          message: `Unexpected text after the closing ' of ${key}`,
        })
      }
      value = raw.slice(1, closing)
    } else {
      const comment = raw.search(/\s#/)
      value = (comment < 0 ? raw : raw.slice(0, comment)).trim()
    }

    const firstLine = firstLineByKey.get(key)
    if (firstLine !== undefined) {
      errors.push({
        line: lineNumber,
        message: `${key} is already set on line ${firstLine}`,
      })
      continue
    }
    firstLineByKey.set(key, lineNumber)
    entries.push({ key, value })
  }

  return { entries, errors }
}

type Variable = Pick<
  EnvironmentVariableResponse,
  'id' | 'key' | 'environments' | 'include_in_preview' | 'is_secret'
>
type Environment = Pick<EnvironmentResponse, 'id' | 'name' | 'is_preview'>

/** A variable as the raw editor sees it in one environment. */
export interface RawEditorVariable {
  variable: Variable
  /** Applies only because every preview environment inherits it. */
  inherited: boolean
}

/**
 * The variables that reach `environment`, one per key. An explicit binding
 * shadows a preview-inherited variable with the same key, matching what the
 * table shows for the environment.
 */
export function rawEditorVariables(
  variables: readonly Variable[],
  environment: Environment
): RawEditorVariable[] {
  const byKey = new Map<string, RawEditorVariable>()
  for (const variable of variables) {
    const explicit = variable.environments.some(
      (entry) => entry.id === environment.id
    )
    const inherited =
      !explicit && environment.is_preview && variable.include_in_preview
    if (!explicit && !inherited) continue
    const current = byKey.get(variable.key)
    if (current && !current.inherited) continue
    byKey.set(variable.key, { variable, inherited: !explicit })
  }
  return [...byKey.values()].sort((a, b) =>
    a.variable.key.localeCompare(b.variable.key)
  )
}

export type RawEnvChange =
  | { type: 'create'; key: string; value: string }
  | {
      type: 'update'
      variable: Variable
      value: string
      /**
       * Other environments the stored variable also applies to. The value
       * changes there too, since one stored variable backs all of them.
       */
      alsoAffects: string[]
      /** Writes a new value to a write-only secret. */
      replacesSecret: boolean
    }
  | { type: 'delete'; variable: Variable }
  | {
      type: 'unbind'
      variable: Variable
      /** Environment IDs the variable keeps after removing this one. */
      remainingEnvironmentIds: number[]
    }
  | {
      type: 'blocked'
      variable: Variable
      reason: string
    }

/**
 * Turns edited `.env` text into the per-variable changes that make
 * `environment` match it.
 *
 * `revealed` holds the current plaintext of every non-secret variable in
 * scope. Secrets are never revealed, so they are not part of the text: a
 * secret key that is absent stays untouched, and one that is present gets
 * the typed value (a rotation).
 */
export function planRawEnvChanges({
  scoped,
  revealed,
  entries,
  environment,
}: {
  scoped: readonly RawEditorVariable[]
  revealed: ReadonlyMap<number, string>
  entries: readonly EnvEntry[]
  environment: Environment
}): RawEnvChange[] {
  const changes: RawEnvChange[] = []
  const byKey = new Map(scoped.map((entry) => [entry.variable.key, entry]))
  const editedKeys = new Set<string>()

  for (const { key, value } of entries) {
    editedKeys.add(key)
    const existing = byKey.get(key)
    if (!existing) {
      changes.push({ type: 'create', key, value })
      continue
    }
    const { variable, inherited } = existing
    if (!variable.is_secret && revealed.get(variable.id) === value) continue
    if (variable.is_secret && value === '') {
      // The API refuses to seal an empty secret; say so before applying.
      changes.push({
        type: 'blocked',
        variable,
        reason: `${key} is a secret and cannot be set to an empty value. Remove the line to keep its current value.`,
      })
      continue
    }
    const alsoAffects = inherited
      ? ['every preview environment']
      : variable.environments
          .filter((entry) => entry.id !== environment.id)
          .map((entry) => entry.name)
    if (!inherited && variable.include_in_preview && !environment.is_preview) {
      alsoAffects.push('preview environments')
    }
    changes.push({
      type: 'update',
      variable,
      value,
      alsoAffects,
      replacesSecret: variable.is_secret,
    })
  }

  for (const { variable, inherited } of scoped) {
    if (variable.is_secret || editedKeys.has(variable.key)) continue
    if (!revealed.has(variable.id)) {
      // Never delete what the editor could not show: the user did not remove
      // the line, it was never there.
      changes.push({
        type: 'blocked',
        variable,
        reason: `The current value of ${variable.key} could not be loaded, so it is left unchanged.`,
      })
      continue
    }
    if (inherited) {
      changes.push({
        type: 'blocked',
        variable,
        reason: `${variable.key} is inherited by every preview environment. Turn off preview inheritance for it in the table to remove it.`,
      })
      continue
    }
    const remainingEnvironmentIds = variable.environments
      .map((entry) => entry.id)
      .filter((id) => id !== environment.id)
    if (remainingEnvironmentIds.length === 0 && !variable.include_in_preview) {
      changes.push({ type: 'delete', variable })
    } else {
      changes.push({ type: 'unbind', variable, remainingEnvironmentIds })
    }
  }

  return changes
}
