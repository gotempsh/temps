// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Keeps an unsaved form in sessionStorage while the user leaves to set up a
 * prerequisite (e.g. a notification provider) and comes back via `returnTo`.
 *
 * Drafts are written only when the user follows such a link, read once when
 * the form mounts, and are tab-scoped by sessionStorage. Storage being
 * unavailable (private mode, quota) silently degrades to "no draft".
 */

const PREFIX = 'temps:form-draft:'

export function saveFormDraft(key: string, value: unknown): void {
  try {
    window.sessionStorage.setItem(PREFIX + key, JSON.stringify(value))
  } catch {
    /* storage unavailable */
  }
}

export function readFormDraft(key: string): unknown {
  try {
    const raw = window.sessionStorage.getItem(PREFIX + key)
    return raw === null ? null : JSON.parse(raw)
  } catch {
    return null
  }
}

export function clearFormDraft(key: string): void {
  try {
    window.sessionStorage.removeItem(PREFIX + key)
  } catch {
    /* storage unavailable */
  }
}

type PlainObject = Record<string, unknown>

function isPlainObject(value: unknown): value is PlainObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isPrimitive(value: unknown): boolean {
  return (
    value === null ||
    typeof value === 'string' ||
    typeof value === 'number' ||
    typeof value === 'boolean'
  )
}

function mergeObject(base: PlainObject, draft: PlainObject): PlainObject {
  const merged: PlainObject = { ...base }
  for (const key of new Set([...Object.keys(base), ...Object.keys(draft)])) {
    const baseValue = base[key]
    const draftValue = draft[key]
    if (draftValue === undefined) continue
    if (!(key in base)) {
      // Optional fields the defaults leave out (e.g. trigger thresholds).
      if (isPrimitive(draftValue)) merged[key] = draftValue
    } else if (baseValue === null || baseValue === undefined) {
      if (isPrimitive(draftValue)) merged[key] = draftValue
    } else if (Array.isArray(baseValue)) {
      if (Array.isArray(draftValue)) merged[key] = draftValue
    } else if (isPlainObject(baseValue)) {
      if (isPlainObject(draftValue))
        merged[key] = mergeObject(baseValue, draftValue)
    } else if (typeof draftValue === typeof baseValue) {
      merged[key] = draftValue
    }
  }
  return merged
}

/**
 * Lays a saved draft over a form's starting values, field by field.
 *
 * Deliberately not validated with the form's submit schema: a draft is an
 * unfinished form (an empty name, a missing metric), and rejecting it would
 * throw away every other field the user had already filled in. Each field
 * only has to have the same shape as its starting value; the submit schema
 * still runs when the user saves.
 */
export function mergeFormDraft<T extends object>(base: T, draft: unknown): T {
  if (!isPlainObject(draft)) return base
  return mergeObject(base as PlainObject, draft) as T
}
