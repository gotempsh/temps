// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EvictionUnconfirmedContainer,
  NodeEvictionResponse,
} from '@/api/client'

/** A sandbox the eviction could not destroy; retrying picks it up. */
export type EvictionFailure = { sandbox_id: string; reason: string }

/**
 * What one eviction did, from either a full success (200) or a partial one
 * (503 with problem extension members). Rendered the same way in both cases
 * so the operator always gets the cleanup commands on screen.
 */
export interface EvictionReport {
  destroyed: string[]
  containersUnconfirmed: EvictionUnconfirmedContainer[]
  failed: EvictionFailure[]
  /** Some sandboxes could not be destroyed. */
  partial: boolean
  /**
   * The server's sentence. Shown when a partial eviction comes from a server
   * that predates the structured members, so nothing is lost.
   */
  detail?: string
}

/** Problem `type` for removing a node that still hosts live sandboxes (409). */
export const NODE_HOSTS_SANDBOXES_TYPE =
  'https://temps.sh/probs/node-hosts-sandboxes'

/** Problem `type` for a partial eviction (HTTP 503). */
export const EVICTION_INCOMPLETE_TYPE =
  'https://temps.sh/probs/sandbox-node-eviction-incomplete'

/**
 * The generated client throws only the Problem body, which carries no
 * `status` unless the server set one. Attach the HTTP status so callers can
 * tell a 409 from a 503 without guessing from the text.
 */
export function withHttpStatus(
  body: unknown,
  status: number | undefined
): Record<string, unknown> {
  const base: Record<string, unknown> =
    body && typeof body === 'object'
      ? { ...(body as Record<string, unknown>) }
      : typeof body === 'string' && body.length > 0
        ? { detail: body }
        : {}
  if (typeof base.status !== 'number' && status !== undefined) {
    base.status = status
  }
  return base
}

function field(error: unknown, key: string): unknown {
  return error && typeof error === 'object'
    ? (error as Record<string, unknown>)[key]
    : undefined
}

function strings(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((v): v is string => typeof v === 'string')
    : []
}

function unconfirmedList(value: unknown): EvictionUnconfirmedContainer[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((v) => {
    const sandbox_id = field(v, 'sandbox_id')
    const cleanup_command = field(v, 'cleanup_command')
    const reason = field(v, 'reason')
    return typeof sandbox_id === 'string' && typeof cleanup_command === 'string'
      ? [
          {
            sandbox_id,
            cleanup_command,
            reason: typeof reason === 'string' ? reason : '',
          },
        ]
      : []
  })
}

function failedList(value: unknown): EvictionFailure[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((v) => {
    const sandbox_id = field(v, 'sandbox_id')
    const reason = field(v, 'reason')
    return typeof sandbox_id === 'string'
      ? [{ sandbox_id, reason: typeof reason === 'string' ? reason : '' }]
      : []
  })
}

export function evictionReportFromResponse(
  data: NodeEvictionResponse
): EvictionReport {
  return {
    destroyed: data.destroyed ?? [],
    containersUnconfirmed: data.containers_unconfirmed ?? [],
    failed: [],
    partial: false,
  }
}

/**
 * Whether an eviction error is the 503 "some sandboxes were left". Matched
 * on the problem type: a 503 for any other reason (the sandbox subsystem is
 * unavailable, say) destroyed nothing and must not read as a partial
 * eviction.
 */
export function isPartialEviction(error: unknown): boolean {
  return field(error, 'type') === EVICTION_INCOMPLETE_TYPE
}

/** Whether another eviction of the same node is already running (409). */
export function isEvictionInProgress(error: unknown): boolean {
  return field(error, 'status') === 409
}

/**
 * Build the report for a partial eviction. The extension members are read
 * defensively: older servers send only `detail`, which is then shown as is.
 * Returns `null` for any other error.
 */
export function evictionReportFromProblem(
  error: unknown
): EvictionReport | null {
  if (!isPartialEviction(error)) return null
  const detail = field(error, 'detail')
  return {
    destroyed: strings(field(error, 'destroyed')),
    containersUnconfirmed: unconfirmedList(
      field(error, 'containers_unconfirmed')
    ),
    failed: failedList(field(error, 'failed')),
    partial: true,
    detail: typeof detail === 'string' && detail.length > 0 ? detail : undefined,
  }
}

/** Anything worth keeping on screen after the toast is gone. */
export function evictionReportNeedsAttention(report: EvictionReport): boolean {
  return (
    report.partial ||
    report.containersUnconfirmed.length > 0 ||
    report.failed.length > 0
  )
}

/** Whether the structured members came back (vs. only a `detail` sentence). */
export function evictionReportHasDetails(report: EvictionReport): boolean {
  return (
    report.destroyed.length > 0 ||
    report.containersUnconfirmed.length > 0 ||
    report.failed.length > 0
  )
}
