// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for external-service health endpoints. Replace with the
 * generated SDK (`bun run openapi-ts`) once the OpenAPI spec is re-exported.
 *
 * TODO(sdk-regen): replace with generated helpers for
 *   - GET /external-services/{id}/health-status
 */

export type HealthStatus = 'operational' | 'degraded' | 'down'

export interface HealthCheckEntry {
  checked_at: string
  status: HealthStatus
  response_time_ms?: number
  error_message?: string
}

export interface ServiceHealthResponse {
  service_id: number
  status?: HealthStatus | null
  last_checked_at?: string | null
  last_error?: string | null
  consecutive_failures: number
  response_time_ms?: number | null
  uptime_24h_percent?: number | null
  recent_checks: HealthCheckEntry[]
}

export interface ServiceHealthStatusEntry {
  service_id: number
  status?: HealthStatus | null
  last_checked_at?: string | null
  consecutive_failures: number
}

export interface ServiceHealthStatusBatch {
  statuses: ServiceHealthStatusEntry[]
}

/**
 * Fetch the current health status for many services in one request.
 * Used on the Storage list page so we don't fan out one GET per row.
 */
export async function listServiceHealthStatuses(
  ids: number[],
): Promise<Map<number, ServiceHealthStatusEntry>> {
  const qs = ids.length > 0 ? `?ids=${ids.join(',')}` : ''
  const response = await fetch(
    `/api/external-services/health-status-batch${qs}`,
    { credentials: 'include' },
  )
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as { detail?: string; title?: string }
      detail = body.detail || body.title || detail
    } catch {
      // fall through
    }
    throw new Error(detail)
  }
  const batch = (await response.json()) as ServiceHealthStatusBatch
  const map = new Map<number, ServiceHealthStatusEntry>()
  for (const entry of batch.statuses) {
    map.set(entry.service_id, entry)
  }
  return map
}

export async function getServiceHealthStatus(
  id: number,
  limit = 50,
): Promise<ServiceHealthResponse> {
  const response = await fetch(
    `/api/external-services/${id}/health-status?limit=${limit}`,
    { credentials: 'include' },
  )
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as { detail?: string; title?: string }
      detail = body.detail || body.title || detail
    } catch {
      // fall through
    }
    throw new Error(detail)
  }
  return (await response.json()) as ServiceHealthResponse
}

/**
 * Trigger a synchronous health check on the backend. Uses the same probe +
 * alert logic as the background monitor, so manual checks stay consistent
 * with periodic ones. Returns the fresh snapshot.
 */
export async function triggerServiceHealthCheck(
  id: number,
): Promise<ServiceHealthResponse> {
  const response = await fetch(`/api/external-services/${id}/health-check`, {
    method: 'POST',
    credentials: 'include',
  })
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as { detail?: string; title?: string }
      detail = body.detail || body.title || detail
    } catch {
      // fall through
    }
    throw new Error(detail)
  }
  return (await response.json()) as ServiceHealthResponse
}
