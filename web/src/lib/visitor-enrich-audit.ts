// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Presentation helpers for the `VISITOR_ENRICHED` audit operation.
 *
 * A deployment token can enrich visitors of its own project. Such a record has
 * no `users` row, so the console would otherwise attribute it to an italic
 * "system" — the operator could not tell which token wrote to their analytics.
 * The token's identity lives in the audit payload instead, the same way plugin
 * activity carries its own actor (see `@/lib/plugin-audit-actor`).
 *
 * The payload is operator-supplied data that has been through the database, so
 * every field is treated as untrusted: anything missing or malformed falls back
 * to the generic presentation rather than crashing the audit page.
 */

export const VISITOR_ENRICHED_OPERATION = 'VISITOR_ENRICHED'

/** How many key names the description lists before collapsing to "+N more". */
const MAX_LISTED_KEYS = 3
const MAX_KEY_LENGTH = 40
const MAX_TOKEN_NAME_LENGTH = 60

function truncate(value: string, maxLength: number): string {
  return value.length > maxLength ? `${value.slice(0, maxLength - 1)}…` : value
}

/** The payload as a plain object, tolerating a JSON-text `data` column. */
function asRecord(data: unknown): Record<string, unknown> | null {
  if (typeof data === 'string') {
    try {
      return asRecord(JSON.parse(data))
    } catch {
      return null
    }
  }
  if (!data || typeof data !== 'object' || Array.isArray(data)) return null
  return data as Record<string, unknown>
}

function readNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

function readKeyNames(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.filter(
    (entry): entry is string => typeof entry === 'string' && entry.length > 0
  )
}

export interface DeploymentTokenAuditActor {
  /** `null` when the payload did not record one. */
  id: number | null
  /** `null` when the token was unnamed or the payload is malformed. */
  name: string | null
}

/**
 * The deployment token behind a visitor enrichment, when there is one.
 *
 * Returns `null` for every other operation and for user-driven enrichments, so
 * callers keep their existing actor presentation.
 */
export function deploymentTokenAuditActor(
  operation: string,
  data: unknown
): DeploymentTokenAuditActor | null {
  if (operation !== VISITOR_ENRICHED_OPERATION) return null
  const record = asRecord(data)
  if (!record || record.actor_kind !== 'deployment_token') return null
  const rawName = record.deployment_token_name
  const name =
    typeof rawName === 'string' && rawName.trim().length > 0
      ? truncate(rawName.trim(), MAX_TOKEN_NAME_LENGTH)
      : null
  return { id: readNumber(record.deployment_token_id), name }
}

/**
 * A one-line description of an enrichment: which visitor, and which keys.
 *
 * Only key *names* are ever shown — enrichment routinely carries personal data
 * and the audit console must not become a second place to read it.
 */
export function describeVisitorEnrichment(data: unknown): string {
  const record = asRecord(data)
  const visitorRowId = readNumber(record?.visitor_row_id)
  const subject = visitorRowId != null ? `visitor ${visitorRowId}` : 'a visitor'

  const keys = readKeyNames(record?.custom_data_keys)
  const reported = readNumber(record?.custom_data_key_count)
  const total =
    reported != null && reported > keys.length ? reported : keys.length

  if (total === 0) return `Enriched ${subject}`

  const listed = keys
    .slice(0, MAX_LISTED_KEYS)
    .map((k) => truncate(k, MAX_KEY_LENGTH))
  if (listed.length === 0) {
    return `Enriched ${subject} (${total} key${total === 1 ? '' : 's'})`
  }

  const remaining = total - listed.length
  const parts = remaining > 0 ? [...listed, `+${remaining} more`] : listed
  return `Enriched ${subject} (${parts.join(', ')})`
}
