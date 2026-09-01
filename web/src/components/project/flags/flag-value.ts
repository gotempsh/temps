// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FlagEnvironmentResponse, FlagResponse } from '@/api/client'

export type FlagValueType = 'bool' | 'string' | 'number' | 'json'

export const FLAG_VALUE_TYPES: {
  value: FlagValueType
  label: string
  hint: string
}[] = [
  { value: 'bool', label: 'Boolean', hint: 'On or off' },
  { value: 'string', label: 'String', hint: 'Any text' },
  { value: 'number', label: 'Number', hint: 'Numeric value' },
  { value: 'json', label: 'JSON', hint: 'Object or array' },
]

/** Where the value an app actually receives came from. */
export type ValueSource = 'override' | 'default' | 'disabled'

export interface EffectiveValue {
  value: unknown
  source: ValueSource
}

/**
 * Narrow a server-supplied value type.
 *
 * An unknown type falls back to `string`, not `bool`: rendering an unrecognised
 * type as a switch would show a control that silently means the wrong thing.
 * Text is the honest degradation — it displays whatever arrived.
 */
export function asFlagValueType(raw: string): FlagValueType {
  if (
    raw === 'bool' ||
    raw === 'string' ||
    raw === 'number' ||
    raw === 'json'
  ) {
    return raw
  }
  console.warn(
    `[temps-flags] unknown flag value type "${raw}" — rendering as text`
  )
  return 'string'
}

export function environmentOverride(
  flag: FlagResponse,
  environmentId: number | undefined
): FlagEnvironmentResponse | undefined {
  if (environmentId === undefined) return undefined
  return flag.environments?.find((e) => e.environment_id === environmentId)
}

/**
 * What an app running in this environment actually receives.
 *
 * Deliberately mirrors `evaluate()` in `crates/temps-flags/src/eval.rs` and the
 * SDK's `evaluate()` — same order, same precedence. If this drifts, the console
 * shows one value while the app sees another, which is worse than showing
 * nothing.
 */
export function resolveEffectiveValue(
  flag: FlagResponse,
  environmentId: number | undefined
): EffectiveValue {
  const override = environmentOverride(flag, environmentId)

  // No override row: the flag is live and inherits its default.
  if (!override) return { value: flag.default_value, source: 'default' }

  // The kill switch outranks everything, including a stored override.
  if (!override.enabled)
    return { value: flag.default_value, source: 'disabled' }

  if (override.value === null || override.value === undefined) {
    return { value: flag.default_value, source: 'default' }
  }

  return { value: override.value, source: 'override' }
}

/** Render a flag value for display. Never returns an empty string. */
export function formatFlagValue(value: unknown, type: FlagValueType): string {
  if (value === null || value === undefined) return '—'
  if (type === 'bool') return value === true ? 'On' : 'Off'
  if (type === 'string') return String(value)
  if (type === 'number') return String(value)
  return JSON.stringify(value)
}

/** Render a flag value for an editable text control. */
export function toEditableString(value: unknown, type: FlagValueType): string {
  if (value === null || value === undefined) return ''
  if (type === 'json') return JSON.stringify(value, null, 2)
  return String(value)
}

export type ParseResult =
  { ok: true; value: unknown } | { ok: false; error: string }

/**
 * Parse text into the flag's declared type.
 *
 * The server rejects a mismatch, so parsing here is what turns a 400 into an
 * inline field error the user can actually act on.
 */
export function parseFlagValue(raw: string, type: FlagValueType): ParseResult {
  const trimmed = raw.trim()

  switch (type) {
    case 'bool':
      if (trimmed === 'true') return { ok: true, value: true }
      if (trimmed === 'false') return { ok: true, value: false }
      return { ok: false, error: 'Must be true or false.' }

    case 'number': {
      if (trimmed === '') return { ok: false, error: 'Enter a number.' }
      const n = Number(trimmed)
      if (!Number.isFinite(n))
        return { ok: false, error: 'Not a valid number.' }
      return { ok: true, value: n }
    }

    case 'string':
      // An empty string is a legitimate value; only null is disallowed.
      return { ok: true, value: raw }

    case 'json': {
      if (trimmed === '') return { ok: false, error: 'Enter JSON.' }
      try {
        const parsed = JSON.parse(trimmed)
        if (parsed === null) {
          return { ok: false, error: 'null is not a valid flag value.' }
        }
        return { ok: true, value: parsed }
      } catch {
        return { ok: false, error: 'Not valid JSON.' }
      }
    }
  }
}

/**
 * Pull the RFC 7807 `detail` out of a failed request.
 *
 * The flags API returns messages a user can act on — "first character 'C' must
 * be a-z or 0-9", "Environment 2 does not belong to project 1" — so showing a
 * generic "something went wrong" would be throwing away the useful part.
 */
export function flagErrorMessage(error: unknown, fallback: string): string {
  const problem = error as { detail?: string; title?: string } | null
  if (problem?.detail) return problem.detail
  if (problem?.title) return problem.title
  if (error instanceof Error && error.message) return error.message
  return fallback
}

/**
 * Validate a flag key against the same rule the server enforces
 * (`[a-z0-9][a-z0-9._-]*`, max 128), so the error arrives before the request.
 */
export function validateFlagKey(key: string): string | null {
  if (!key) return 'Enter a key.'
  if (key.length > 128) return 'Keys are limited to 128 characters.'
  if (!/^[a-z0-9]/.test(key)) {
    return 'Must start with a lowercase letter or digit.'
  }
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(key)) {
    return 'Only lowercase letters, digits, dot, underscore and hyphen.'
  }
  return null
}
