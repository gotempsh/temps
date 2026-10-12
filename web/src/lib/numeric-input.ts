// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Ref } from 'react'

/**
 * Text-first handling of numeric form fields.
 *
 * A numeric field must hold what the user typed, not a number derived from
 * it: coercing an empty field to a default (`Number('') || 60`) puts the
 * default back mid-edit, so clearing the field and typing `42` ends up as
 * `6042`, or `042` when the default is 0. These helpers keep the raw text
 * separate from the committed number and defer range checks to submit time.
 */

/** What a piece of typed text means as a number. */
export type NumericParse =
  { kind: 'empty' } | { kind: 'invalid' } | { kind: 'number'; value: number }

/** Parse typed text. Empty is a valid transient state, not a value. */
export function parseNumericText(text: string): NumericParse {
  const trimmed = text.trim()
  if (trimmed === '') return { kind: 'empty' }
  const value = Number(trimmed)
  if (!Number.isFinite(value)) return { kind: 'invalid' }
  return { kind: 'number', value }
}

/** The text to show for a committed value; nothing for "no value". */
export function formatNumericText(value: number | null | undefined): string {
  return typeof value === 'number' && Number.isFinite(value)
    ? String(value)
    : ''
}

/**
 * The number a field should commit for the text just typed: a number for
 * valid text, `undefined` for an empty field, and `null` for text that is
 * not a number (nothing should be committed, the previous value stands).
 */
export function committedNumber(text: string): number | undefined | null {
  const parsed = parseNumericText(text)
  if (parsed.kind === 'empty') return undefined
  if (parsed.kind === 'invalid') return null
  return parsed.value
}

/**
 * Whether the text being edited must be replaced because the committed value
 * changed from outside (a form reset, data that finished loading). Text that
 * already means the incoming value, including a half-typed "042" or an
 * emptied field for "no value", is left alone so editing is not disturbed.
 * `isEchoOfOwnEdit` is true when the value arrived as the parent's reaction
 * to an edit made in this very field (see `NumberInput`).
 */
export function shouldResyncNumericText(
  text: string,
  value: number | null | undefined,
  isEchoOfOwnEdit = false
): boolean {
  // What the parent answers to an edit made here is never an outside change.
  if (isEchoOfOwnEdit) return false
  const typed = committedNumber(text)
  return typed === null || typed !== (value ?? undefined)
}

/**
 * Whether `value`, arriving right after this field emitted `emitted`, is the
 * parent merely echoing the edit rather than overriding it.
 *
 * - After emptying the field (`emitted === undefined`) any value is an echo:
 *   react-hook-form answers `undefined` with the field's `defaultValues`
 *   entry, and following that would put the default back into a field the
 *   user just emptied.
 * - After a number was emitted, the same number is an echo. A different
 *   number means the parent clamped or rejected the edit, and the field
 *   should show what the parent decided.
 */
export function isEchoOfOwnEdit(
  emitted: number | undefined,
  value: number | null | undefined
): boolean {
  if (emitted === undefined) return true
  return value === emitted
}

export interface NumericRules {
  min?: number
  max?: number
  /** Reject fractional values. */
  integer?: boolean
  /** An empty field is an error. Default: empty is reported as required. */
  required?: boolean
}

/**
 * Validate a committed value, typically on submit. Returns a user-facing
 * message, or `null` when the value is acceptable. An absent value is only
 * an error when the field is `required` (the default).
 */
export function validateNumeric(
  value: number | null | undefined,
  rules: NumericRules = {}
): string | null {
  const { min, max, integer = false, required = true } = rules
  if (value === null || value === undefined || Number.isNaN(value)) {
    return required ? 'Enter a number' : null
  }
  if (!Number.isFinite(value)) return 'Enter a number'
  if (integer && !Number.isInteger(value)) return 'Enter a whole number'
  if (min !== undefined && value < min) {
    return max !== undefined
      ? `Must be between ${min} and ${max}`
      : `Must be at least ${min}`
  }
  if (max !== undefined && value > max) {
    return min !== undefined
      ? `Must be between ${min} and ${max}`
      : `Must be at most ${max}`
  }
  return null
}

/**
 * Validate several named fields at once and return the first problem,
 * prefixed with the field's label, for a toast or an inline summary.
 */
export function firstNumericError(
  fields: ReadonlyArray<{
    label: string
    value: number | null | undefined
    rules?: NumericRules
  }>
): string | null {
  for (const field of fields) {
    const message = validateNumeric(field.value, field.rules)
    if (message) return `${field.label}: ${message}`
  }
  return null
}

/**
 * Adapter for react-hook-form's `field` object:
 * `<NumberInput {...numberFieldProps(field)} min={1} />`. Emptying the field
 * sets the form value to `undefined`, so the schema (not a hard-coded
 * default) decides whether an empty value is acceptable.
 */
export function numberFieldProps(field: {
  name: string
  value: number | null | undefined
  onChange: (value: number | undefined) => void
  onBlur: () => void
  ref: Ref<HTMLInputElement>
  disabled?: boolean
}) {
  return {
    name: field.name,
    value: field.value,
    onValueChange: field.onChange,
    onBlur: field.onBlur,
    ref: field.ref,
    disabled: field.disabled,
  }
}
