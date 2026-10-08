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

/**
 * What happened once the failure streak reached the alert threshold. The
 * server does not track whether a notification was delivered, so neither
 * does this: it only reports the alarm and whether any destination exists.
 */
export interface ServiceDownAlert {
  alarm_id?: number | null
  alarm_status?: string | null
  alarm_fired_at?: string | null
  silenced_until?: string | null
  /** `null`/absent when the server could not determine it. */
  notifications_configured?: boolean | null
  notification_setup_path: string
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
  down_alert?: ServiceDownAlert | null
}

/** Consecutive failed checks after which the health monitor raises an alarm. */
export const FAILURES_BEFORE_ALERT = 3

export interface ServiceFailureSummary {
  headline: string
  /** What happened to the alert, stated without claiming delivery. */
  alertNote?: string
  /** Where to fix a missing notification destination, with a way back. */
  setupHref?: string
  /** Where to see the raised alarm. */
  alarmHref?: string
}

/**
 * Wording for the health card's failure alert. Separates the three facts an
 * operator needs: the failure was detected, an alarm was (or was not)
 * raised, and whether any notification destination could have received it.
 */
export function serviceFailureSummary(
  health: Pick<ServiceHealthResponse, 'consecutive_failures' | 'down_alert'>,
  serviceId: number,
  formatTime: (iso: string) => string = (iso) => new Date(iso).toLocaleString()
): ServiceFailureSummary {
  const failures = health.consecutive_failures
  if (failures < FAILURES_BEFORE_ALERT) {
    return { headline: `Service has failed ${failures} check(s) in a row.` }
  }
  const headline = `Service has failed ${failures} consecutive checks.`
  const alert = health.down_alert
  if (!alert) return { headline }
  if (alert.alarm_id == null) {
    return {
      headline,
      alertNote: 'No open down alarm is recorded for this service.',
    }
  }
  const alarmHref = '/monitoring/alarms'
  if (alert.silenced_until) {
    return {
      headline,
      alertNote: `A down alarm was raised, but its notifications are silenced until ${formatTime(alert.silenced_until)}.`,
      alarmHref,
    }
  }
  if (alert.notifications_configured === false) {
    const returnTo = encodeURIComponent(`/storage/${serviceId}`)
    return {
      headline,
      alertNote:
        'A down alarm was raised, but no notification provider is configured, so nobody was notified.',
      setupHref: `${alert.notification_setup_path}?returnTo=${returnTo}`,
      alarmHref,
    }
  }
  if (alert.notifications_configured === true) {
    return {
      headline,
      alertNote:
        "A down alarm was raised and routed to your notification providers. Delivery isn't confirmed here; if nothing arrived, test the provider in Settings → Notifications.",
      alarmHref,
    }
  }
  return { headline, alertNote: 'A down alarm was raised.', alarmHref }
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
  ids: number[]
): Promise<Map<number, ServiceHealthStatusEntry>> {
  const qs = ids.length > 0 ? `?ids=${ids.join(',')}` : ''
  const response = await fetch(
    `/api/external-services/health-status-batch${qs}`,
    { credentials: 'include' }
  )
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
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
  limit = 50
): Promise<ServiceHealthResponse> {
  const response = await fetch(
    `/api/external-services/${id}/health-status?limit=${limit}`,
    { credentials: 'include' }
  )
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
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
  id: number
): Promise<ServiceHealthResponse> {
  const response = await fetch(`/api/external-services/${id}/health-check`, {
    method: 'POST',
    credentials: 'include',
  })
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
      detail = body.detail || body.title || detail
    } catch {
      // fall through
    }
    throw new Error(detail)
  }
  return (await response.json()) as ServiceHealthResponse
}
