// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * External-service health helpers over the generated SDK. Types come from the
 * OpenAPI spec; this module only narrows `status` to the values the health
 * monitor writes and keeps the error shape callers already display.
 */

import {
  getServiceHealthStatus as getServiceHealthStatusSdk,
  listServiceHealthStatuses as listServiceHealthStatusesSdk,
  triggerServiceHealthCheck as triggerServiceHealthCheckSdk,
} from '@/api/client'
import type {
  HealthCheckEntryResponse,
  ServiceDownAlertResponse,
  ServiceHealthResponse as ServiceHealthResponseDto,
  ServiceHealthStatusEntryResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'

export type HealthStatus = 'operational' | 'degraded' | 'down'

export type HealthCheckEntry = Omit<HealthCheckEntryResponse, 'status'> & {
  status: HealthStatus
}

/**
 * The alarm raised once the failure streak reached the alert threshold, and
 * how many destinations receive alerts of its severity today. The server does
 * not track whether a notification was delivered, so neither does this.
 */
export type ServiceDownAlert = ServiceDownAlertResponse

export type ServiceHealthResponse = Omit<
  ServiceHealthResponseDto,
  'status' | 'recent_checks'
> & {
  status?: HealthStatus | null
  recent_checks: HealthCheckEntry[]
}

export type ServiceHealthStatusEntry = Omit<
  ServiceHealthStatusEntryResponse,
  'status'
> & {
  status?: HealthStatus | null
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
 * Wording for the health card's failure alert. Separates the facts an
 * operator needs — the failure was detected, an alarm was (or was not)
 * raised, and who receives alerts of its severity *now* — and never claims
 * that this particular alarm was routed or delivered: the destination count
 * describes current configuration, which may differ from when it fired.
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
  const severity = alert.alert_severity
  if (alert.silenced_until) {
    return {
      headline,
      alertNote: `A down alarm was raised, but its notifications are silenced until ${formatTime(alert.silenced_until)}.`,
      alarmHref,
    }
  }
  const destinations = alert.notification_destinations
  if (destinations === 0) {
    const returnTo = encodeURIComponent(`/storage/${serviceId}`)
    return {
      headline,
      alertNote: `A down alarm was raised, but no notification destination receives ${severity} alerts, so nobody is being notified.`,
      setupHref: `${alert.notification_setup_path}?returnTo=${returnTo}`,
      alarmHref,
    }
  }
  if (destinations != null) {
    const phrase =
      destinations === 1
        ? '1 notification destination currently receives'
        : `${destinations} notification destinations currently receive`
    return {
      headline,
      alertNote: `A down alarm was raised. ${phrase} ${severity} alerts; Temps doesn't record whether this alarm reached them. If nothing arrived, test the provider in Settings → Notifications.`,
      alarmHref,
    }
  }
  return { headline, alertNote: 'A down alarm was raised.', alarmHref }
}

function healthError(error: unknown, fallback: string): Error {
  return new Error(problemDetail(error, fallback))
}

/**
 * Fetch the current health status for many services in one request.
 * Used on the Storage list page so we don't fan out one GET per row.
 */
export async function listServiceHealthStatuses(
  ids: number[]
): Promise<Map<number, ServiceHealthStatusEntry>> {
  try {
    const { data } = await listServiceHealthStatusesSdk({
      query: ids.length > 0 ? { ids: ids.join(',') } : undefined,
      throwOnError: true,
    })
    return new Map(
      data.statuses.map((entry) => [
        entry.service_id,
        entry as ServiceHealthStatusEntry,
      ])
    )
  } catch (error) {
    throw healthError(error, 'Failed to load service health')
  }
}

export async function getServiceHealthStatus(
  id: number,
  limit = 50
): Promise<ServiceHealthResponse> {
  try {
    const { data } = await getServiceHealthStatusSdk({
      path: { id },
      query: { limit },
      throwOnError: true,
    })
    return data as ServiceHealthResponse
  } catch (error) {
    throw healthError(error, `Failed to load health for service ${id}`)
  }
}

/**
 * Trigger a synchronous health check on the backend. Uses the same probe +
 * alert logic as the background monitor, so manual checks stay consistent
 * with periodic ones. Returns the fresh snapshot.
 */
export async function triggerServiceHealthCheck(
  id: number
): Promise<ServiceHealthResponse> {
  try {
    const { data } = await triggerServiceHealthCheckSdk({
      path: { id },
      throwOnError: true,
    })
    return data as ServiceHealthResponse
  } catch (error) {
    throw healthError(error, `Health check failed for service ${id}`)
  }
}
