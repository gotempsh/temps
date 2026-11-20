// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Turns the raw arguments of a platform change into something a person can
 * review before approving, and turns a rejected change into the one sentence
 * that explains why. Both are pure so they can be tested without rendering.
 */

/** The marker the server puts where a value was redacted or is a secret handle. */
const MASKED = '***'

/** The deepest object nesting that is spelled out; deeper values stay compact. */
const MAX_DEPTH = 2

export type ParameterValue = { kind: 'text'; text: string } | { kind: 'hidden' }

export interface ParameterRow {
  /** The API field name, `parent.child` for nested values. */
  key: string
  /** The field name written for people: "Bucket name". */
  label: string
  value: ParameterValue
}

/** `bucket_name` -> `Bucket name`, `s3_source_id` -> `S3 source id`. */
export function humanizeKey(key: string): string {
  const words = key.replace(/[_-]+/g, ' ').trim()
  if (words === '') return key
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function isMasked(value: unknown): boolean {
  return value === MASKED || isSecretHandle(value)
}

function isSecretHandle(value: unknown): boolean {
  return typeof value === 'string' && value.startsWith('secret_ref:v1:')
}

function scalarText(value: unknown): string {
  if (typeof value === 'boolean') return value ? 'Yes' : 'No'
  return String(value)
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function rowsFor(
  value: Record<string, unknown>,
  prefix: string,
  depth: number
): ParameterRow[] {
  const rows: ParameterRow[] = []
  for (const [name, entry] of Object.entries(value)) {
    if (entry === null || entry === undefined) continue
    const key = prefix === '' ? name : `${prefix}.${name}`
    const label = humanizeKey(name)
    if (isMasked(entry)) {
      rows.push({ key, label, value: { kind: 'hidden' } })
    } else if (isPlainObject(entry) && depth < MAX_DEPTH) {
      const nested = rowsFor(entry, key, depth + 1)
      if (nested.length > 0) rows.push(...nested)
    } else if (Array.isArray(entry)) {
      const text =
        entry.length === 0
          ? 'None'
          : entry
              .map((item) =>
                typeof item === 'object' && item !== null
                  ? JSON.stringify(item)
                  : scalarText(item)
              )
              .join(', ')
      rows.push({ key, label, value: { kind: 'text', text } })
    } else if (typeof entry === 'object') {
      rows.push({
        key,
        label,
        value: { kind: 'text', text: JSON.stringify(entry) },
      })
    } else {
      rows.push({
        key,
        label,
        value: { kind: 'text', text: scalarText(entry) },
      })
    }
  }
  return rows
}

/** One row per value the change will send, in the order the model wrote them. */
export function describeParameters(parameters: unknown): ParameterRow[] {
  if (!isPlainObject(parameters)) return []
  return rowsFor(parameters, '', 0)
}

/** True when any value of the change is a hidden credential. */
export function hasHiddenValue(rows: ParameterRow[]): boolean {
  return rows.some((row) => row.value.kind === 'hidden')
}

/** `Create a new S3 source (POST /backups/s3-sources)` -> `Create a new S3 source`. */
export function summaryTitle(
  summary: string | null | undefined
): string | null {
  const title = summary?.replace(/\s*\([A-Z]+ [^)]*\)\s*$/, '').trim()
  return title ? title : null
}

/**
 * The reason a platform change was rejected. A failed change reports
 * `Upstream call to operation 'x' returned HTTP 400: {"detail": ...}`; the
 * person reading it wants the `detail`, not the envelope around it.
 */
export function upstreamFailureMessage(error: string): string {
  const separator = error.indexOf(': {')
  if (separator === -1) return error
  try {
    const body = JSON.parse(error.slice(separator + 2)) as unknown
    if (isPlainObject(body)) {
      const detail = typeof body.detail === 'string' ? body.detail : null
      const title = typeof body.title === 'string' ? body.title : null
      if (detail && title) return `${title}: ${detail}`
      if (detail ?? title) return detail ?? title ?? error
    }
  } catch {
    // Not a problem document; the whole message is the best explanation.
  }
  return error
}
