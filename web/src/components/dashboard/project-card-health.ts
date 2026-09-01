// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Health indicator shown on every project card and in the project header.
 *
 * Two independent signals feed this, and the order matters:
 *
 * 1. **Uptime monitors** (`/monitors-health/projects`) — the latest production
 *    status check. This is what "is the project healthy" actually means, and
 *    it has an answer even when nobody visited the site.
 * 2. **User traffic** (`/proxy-logs/stats/projects-health`) — the proxy error
 *    rate over the window. It measures *real user requests* and deliberately
 *    excludes Temps' own monitor checks (`is_system_request = FALSE`), so a
 *    perfectly healthy but quiet project produces zero requests and the
 *    backend reports `"unknown"` because it cannot divide by zero.
 *
 * Reading only (2) is why a project with a monitor sitting at 100% uptime used
 * to be labelled "unknown": its uptime checks were filtered out of the very
 * query being asked, and no human happened to visit in the last hour. Monitors
 * therefore win whenever one exists; traffic is the fallback for projects that
 * have no monitor configured.
 *
 * Whatever the inputs, this never resolves to "render nothing" — "we could not
 * measure it" is itself a state the operator needs to see.
 */

export type ProjectHealthTone =
  'healthy' | 'degraded' | 'down' | 'idle' | 'unavailable' | 'pending'

export interface ProjectHealthIndicator {
  tone: ProjectHealthTone
  /** Short label rendered next to the dot. */
  label: string
  /** Full sentence explaining what the label means and how it was derived. */
  detail: string
}

/** The slice of `ProjectHealthSummary` this indicator reads. */
export interface ProjectHealthInput {
  status: string
  total_requests: number
  total_errors: number
  error_rate: number
  avg_response_time_ms: number
}

/** The slice of `ProjectMonitorHealth` this indicator reads. */
export interface ProjectMonitorHealthInput {
  /** `operational` | `degraded` | `down` | `no_monitors` */
  status: string
}

export interface ProjectHealthIndicatorOptions {
  health?: ProjectHealthInput
  loading?: boolean
  error?: boolean
  /** Latest production uptime-monitor status, when monitors are readable. */
  monitor?: ProjectMonitorHealthInput
  /** Hours the summary covers, for the explanatory detail text. */
  windowHours?: number
}

function windowLabel(hours: number): string {
  if (hours === 24) return 'the last 24 hours'
  if (hours === 1) return 'the last hour'
  return `the last ${hours} hours`
}

function measuredDetail(
  health: ProjectHealthInput,
  windowHours: number,
  lead: string
): string {
  const requests = health.total_requests.toLocaleString()
  const errors = health.total_errors.toLocaleString()
  const latency = Math.round(health.avg_response_time_ms).toLocaleString()
  return (
    `${lead} ${requests} requests over ${windowLabel(windowHours)}, ` +
    `${errors} server errors (${health.error_rate}%), ${latency} ms average response time.`
  )
}

/**
 * A configured production monitor is the direct answer to "is this healthy",
 * so it outranks traffic — including when traffic is silent. `no_monitors`
 * means the project opted out, not that it is unhealthy, so it declines to
 * answer and lets the traffic signal decide.
 */
function monitorIndicator(
  monitor: ProjectMonitorHealthInput
): ProjectHealthIndicator | null {
  switch (monitor.status) {
    case 'operational':
      return {
        tone: 'healthy',
        label: 'Healthy',
        detail:
          'The production uptime monitor reports every check operational.',
      }
    case 'degraded':
      return {
        tone: 'degraded',
        label: 'Degraded',
        detail:
          'Some production uptime monitors are failing their checks. Open Monitors to see which.',
      }
    case 'down':
      return {
        tone: 'down',
        label: 'Down',
        detail:
          'Every production uptime monitor is failing its checks, or has not reported in over a day.',
      }
    default:
      // 'no_monitors', or a status this build does not know about.
      return null
  }
}

export function projectHealthIndicator({
  health,
  loading = false,
  error = false,
  monitor,
  windowHours = 24,
}: ProjectHealthIndicatorOptions): ProjectHealthIndicator {
  // Checked before the traffic branches — a monitor answers even when the
  // traffic query failed or came back empty, which is the whole point of it.
  if (monitor) {
    const fromMonitor = monitorIndicator(monitor)
    if (fromMonitor) return fromMonitor
  }

  // An outright failure is reported as a failure. Falling back to a grey dot
  // would read as "this project is quiet" when we simply could not ask.
  if (error) {
    return {
      tone: 'unavailable',
      // Short enough to sit beside a long project name; the detail carries the
      // rest, and is exposed to screen readers rather than hidden in a tooltip.
      label: 'Unavailable',
      detail:
        'Temps could not load request health for this project. Reload the page to try again; if it persists, check that the proxy is running.',
    }
  }

  if (loading && !health) {
    return {
      tone: 'pending',
      label: 'Checking…',
      detail: `Loading request health for ${windowLabel(windowHours)}.`,
    }
  }

  if (!health) {
    return {
      tone: 'unavailable',
      label: 'No health data',
      detail:
        'The health summary returned no entry for this project, so its status could not be determined.',
    }
  }

  // Zero requests is a real, reportable state — not a missing one. The backend
  // labels it "unknown" because it cannot compute an error rate from nothing.
  if (health.total_requests === 0) {
    return {
      tone: 'idle',
      label: 'No traffic',
      detail:
        `No user requests reached this project in ${windowLabel(windowHours)}, so there is no traffic to measure. ` +
        'Add an uptime monitor to get a health signal that does not depend on visitors.',
    }
  }

  switch (health.status) {
    case 'healthy':
    case 'operational':
      return {
        tone: 'healthy',
        label: 'Healthy',
        detail: measuredDetail(health, windowHours, 'Serving normally:'),
      }
    case 'degraded':
      return {
        tone: 'degraded',
        label: 'Degraded',
        detail: measuredDetail(
          health,
          windowHours,
          'Elevated error rate over 10%:'
        ),
      }
    case 'down':
      return {
        tone: 'down',
        label: 'Down',
        detail: measuredDetail(
          health,
          windowHours,
          'More than half of requests are failing:'
        ),
      }
    default:
      // A status this build does not know about still has real traffic behind
      // it, so report the numbers rather than pretending the project is idle.
      return {
        tone: 'unavailable',
        label: 'Unknown',
        detail: measuredDetail(
          health,
          windowHours,
          `Unrecognized health status "${health.status}".`
        ),
      }
  }
}
