// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MonitorResponse, RuntimeHealthSummary } from '@/api/client'
import type { StatusTone } from '@temps-sdk/ds'

export function runtimeStateTone(state: string): StatusTone {
  if (state === 'healthy') return 'ok'
  if (state === 'unhealthy') return 'error'
  return 'idle'
}

export function formatRuntimePercent(value: number | null | undefined): string {
  return value == null ? '—' : `${value.toFixed(1)}%`
}

export function runtimeHealthExplanation(
  health: RuntimeHealthSummary
): string | undefined {
  if (!health.configured)
    return 'Redeploy this environment to save its effective probe configuration. Existing deployment readiness is not runtime uptime.'
  if (health.reason === 'runtime_exec_unavailable')
    return 'Exec runs during the deployment gate only. Continuous exec checks are unavailable until probe processes can be contained without stopping the application. Choose process, HTTP or TCP in deployment settings, then redeploy.'
  if (health.reason === 'runtime_configuration_invalid')
    return 'The saved deployment probe exceeds the runtime safety limits. Use a 1–30 second timeout and a 1–3600 second interval in deployment settings, then redeploy.'
  if (health.reason === 'paused' || health.reason === 'disabled')
    return 'Monitoring is paused. This time is excluded from uptime. A worker that cannot receive revocation may finish its existing read-only lease, for at most 120 seconds.'
  if (health.reason === 'sleeping')
    return 'This environment is sleeping intentionally. Sleeping time is excluded from uptime.'
  if (health.state === 'unknown')
    return 'Fresh authorized observations are missing or the process is stabilizing. Unknown time reduces coverage and is not counted as an outage or a success. Check the assigned worker connection, mTLS enrollment and probe settings.'
  return undefined
}

export function visibleExternalMonitors(
  monitors: MonitorResponse[],
  workerEnvironments: ReadonlySet<number>
): MonitorResponse[] {
  return monitors.filter(
    (monitor) =>
      !(
        monitor.is_managed === true &&
        monitor.environment_id != null &&
        workerEnvironments.has(monitor.environment_id)
      )
  )
}
