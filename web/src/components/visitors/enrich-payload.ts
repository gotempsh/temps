// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers behind the visitor "Enrich Visitor" dialog.
 *
 * `PUT /analytics/visitors/{visitor_id}/enrich` *merges* the body's top-level
 * `custom_data` keys into the stored document, and a key whose value is JSON
 * `null` removes that key. The dialog pre-fills the editor with the visitor's
 * whole existing `custom_data` and submits the edited document, so a key the
 * operator deleted in the editor has to be sent back as an explicit `null` —
 * otherwise the merge silently keeps it and the deletion appears to do nothing.
 */

/** Top-level keys of `value`, or `[]` when it is not a plain JSON object. */
function topLevelKeys(value: unknown): string[] {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return []
  return Object.keys(value as Record<string, unknown>)
}

/**
 * Turn "the document the operator now wants" into a merge request: every key
 * that existed before but is absent from the edited document is sent as `null`
 * so the server removes it.
 */
export function buildEnrichPayload(
  existing: unknown,
  edited: Record<string, unknown>
): Record<string, unknown> {
  const payload: Record<string, unknown> = { ...edited }
  for (const key of topLevelKeys(existing)) {
    if (!Object.prototype.hasOwnProperty.call(edited, key)) {
      payload[key] = null
    }
  }
  return payload
}

const PAYLOAD_TOO_LARGE_PATTERN =
  /payload too large|length limit exceeded|request entity too large|body too large/i

/**
 * Whether a rejected enrich request is the 16 KB body limit.
 *
 * The generated client throws the parsed error body (an RFC 7807 problem) or,
 * when the rejection is not JSON, the raw response text — the HTTP status is
 * not always on the thrown value, so both shapes are checked.
 */
export function isPayloadTooLargeError(error: unknown): boolean {
  if (typeof error === 'string') return PAYLOAD_TOO_LARGE_PATTERN.test(error)
  if (!error || typeof error !== 'object') return false
  const candidate = error as Record<string, unknown>
  for (const key of ['status', 'statusCode', 'code']) {
    if (candidate[key] === 413 || candidate[key] === '413') return true
  }
  for (const key of ['detail', 'title', 'message', 'error']) {
    const value = candidate[key]
    if (typeof value === 'string' && PAYLOAD_TOO_LARGE_PATTERN.test(value)) {
      return true
    }
  }
  return false
}
