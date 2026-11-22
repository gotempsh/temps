// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Tool the assistant calls to ask the user for a credential. */
export const REQUEST_SECRET_TOOL_NAME = 'request_secret'

/**
 * What the chat says on the user's behalf once they have saved the private
 * form. The assistant told them it was waiting for exactly this.
 */
export const SECRETS_PROVIDED_MESSAGE =
  "I've filled in the private form. Please continue."

/** Every handle starts with this; the id follows. */
export const SECRET_REF_PREFIX = 'secret_ref:v1:'

export function isRequestSecretTool(name: string): boolean {
  return (
    name === REQUEST_SECRET_TOOL_NAME ||
    name.endsWith(`__${REQUEST_SECRET_TOOL_NAME}`)
  )
}

/** One input of the form. The credential itself is never part of it. */
export interface SecretRequestItem {
  /** The id inside the handle; what the secret endpoints are keyed by. */
  id: string
  /** What the input is, in plain words ("Secret access key"). */
  label: string
  /** The request field it fills, for the small mono hint ("secret_key"). */
  field: string
  /** The API's own description of the field, shown under the input. */
  help: string | null
}

/**
 * What the assistant asked for: one form for one operation, with an input per
 * credential field. It is the last step of a guided conversation, so it also
 * says which operation it is for and why.
 */
export interface SecretRequest {
  operationId: string
  /** The operation in plain words ("Create a new S3 source"). */
  operationSummary: string | null
  /** One sentence from the assistant on what the form is for. */
  purpose: string | null
  items: SecretRequestItem[]
}

const text = (value: unknown): string | null =>
  typeof value === 'string' && value.trim().length > 0 ? value : null

/**
 * Read the descriptor the `request_secret` tool returned. `null` while the call
 * is still running, and when the result is a refusal sentence rather than a
 * descriptor (an unknown operation, too many open requests).
 */
export function parseSecretRequest(
  result: string | null | undefined
): SecretRequest | null {
  if (!result) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(result)
  } catch {
    return null
  }
  if (typeof parsed !== 'object' || parsed === null) return null
  const record = parsed as Record<string, unknown>
  if (!Array.isArray(record.requests)) return null
  const items: SecretRequestItem[] = []
  for (const entry of record.requests) {
    if (typeof entry !== 'object' || entry === null) continue
    const item = entry as Record<string, unknown>
    const handle = item.handle
    if (typeof handle !== 'string' || !handle.startsWith(SECRET_REF_PREFIX)) {
      continue
    }
    const id = handle.slice(SECRET_REF_PREFIX.length)
    if (id.length === 0) continue
    items.push({
      id,
      label: text(item.label) ?? 'Credential',
      field:
        typeof item.field === 'string' ? item.field.replace(/^\//, '') : '',
      help: text(item.help),
    })
  }
  if (items.length === 0) return null
  return {
    operationId:
      typeof record.operation_id === 'string' ? record.operation_id : '',
    operationSummary: text(record.operation_summary),
    purpose: text(record.purpose),
    items,
  }
}

/** Whether a stored parameter value is a reference to a credential the user provided. */
export function isSecretReference(value: unknown): boolean {
  return typeof value === 'string' && value.startsWith(SECRET_REF_PREFIX)
}

export type SecretRequestState = 'awaiting' | 'filled' | 'closed'

/**
 * Where a request stands, from the chat's list of open requests. A request
 * missing from the list was discarded or expired, so it can no longer be used
 * and the form is not offered.
 */
export function secretRequestState(
  id: string,
  open: ReadonlyArray<{ id: string; status: string }> | undefined
): SecretRequestState | 'unknown' {
  if (!open) return 'unknown'
  const found = open.find((slot) => slot.id === id)
  if (!found) return 'closed'
  if (found.status === 'filled') return 'filled'
  if (found.status === 'awaiting') return 'awaiting'
  return 'closed'
}
