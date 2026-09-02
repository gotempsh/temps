// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helper for the Temps Cloud telemetry backfill status endpoint
 * (ADR-040 §1), which is not yet reflected in the generated OpenAPI client.
 *
 * Reuses the generated `client` transport (baseUrl `/api`, bearer auth, error
 * parsing) rather than hand-rolling `fetch`, so behaviour matches the rest of
 * the SDK — the same approach `lib/on-demand-certs.ts` uses.
 *
 * TODO(sdk-regen): replace with the generated SDK helper for
 *   - GET /otel/cloud-telemetry/backfill/{project_id}
 * once `bun run openapi-ts` is re-run against a server exposing it.
 */

import { queryOptions } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'

/** Mirror of the backend `CloudTelemetryBackfillStatus` enum. */
export type CloudBackfillStatus =
  'not_started' | 'running' | 'completed' | 'failed'

/** Mirror of the backend `CloudTelemetryFidelity` enum. */
export type CloudTelemetryFidelity = 'metered' | 'queryable'

/** Mirror of the backend `CloudBackfillStatusResponse` DTO. */
export interface CloudBackfillStatusResponse {
  project_id: number
  status: CloudBackfillStatus
  fidelity: CloudTelemetryFidelity
  /** False while the project is still at `metered` fidelity. */
  backfill_available: boolean
  spans_processed: number
  spans_total: number
  /** Absent when the total is unknown or zero. */
  percent_complete?: number
  window_from?: string
  window_to?: string
  started_at?: string
  /** Bumped on every progress write — the liveness signal for a running run. */
  updated_at?: string
  completed_at?: string
  last_error?: string
  /** The exact command to run. Present in every state. */
  command: string
  /** Where to raise fidelity, when `backfill_available` is false. */
  setup_path?: string
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

/**
 * How long a `running` backfill may go without a progress write before the UI
 * calls it stalled.
 *
 * The CLI writes once per chunk, and a chunk is a day of spans, so a few
 * minutes of silence is normal and ten is not. Calling it stalled is better
 * than a spinner that never resolves: the operator can go and look at the
 * terminal, or just re-run — the backfill is resumable and idempotent.
 */
export const BACKFILL_STALL_THRESHOLD_MS = 10 * 60 * 1000

export async function getCloudBackfillStatus(
  projectId: number,
  signal?: AbortSignal
): Promise<CloudBackfillStatusResponse> {
  const { data } = await client.get<CloudBackfillStatusResponse, unknown, true>(
    {
      security: [...BEARER_SECURITY],
      url: '/otel/cloud-telemetry/backfill/{project_id}',
      path: { project_id: projectId },
      signal,
      throwOnError: true,
    }
  )
  return data
}

export function getCloudBackfillStatusOptions(projectId: number | undefined) {
  return queryOptions({
    queryKey: ['getCloudBackfillStatus', projectId],
    queryFn: ({ signal }) =>
      getCloudBackfillStatus(projectId as number, signal),
    enabled: typeof projectId === 'number',
    // A backfill is driven by another process, so the only way the Console
    // learns it moved is by asking again. Cheap: one indexed single-row read.
    refetchInterval: (query) =>
      query.state.data?.status === 'running' ? 5000 : false,
  })
}

/**
 * Whether a `running` backfill has gone quiet for long enough that the process
 * driving it is probably gone.
 */
export function isBackfillStalled(
  status: CloudBackfillStatusResponse | undefined,
  now: number = Date.now()
): boolean {
  if (!status || status.status !== 'running' || !status.updated_at) return false
  const updatedAt = new Date(status.updated_at).getTime()
  if (Number.isNaN(updatedAt)) return false
  return now - updatedAt > BACKFILL_STALL_THRESHOLD_MS
}
