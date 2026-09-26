// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const CONTAINER_LOGS_MARKER = 'Container logs for unhealthy/stopped services:'
const MAX_FAILURE_SUMMARY_LENGTH = 360

export interface DeploymentFailureSummary {
  fullReason: string
  summary: string
  hasMore: boolean
}

export function deploymentFailureSummary(
  rawReason: string
): DeploymentFailureSummary {
  const fullReason = rawReason.replace(/\\n/g, '\n').trim()
  const logsStart = fullReason.indexOf(CONTAINER_LOGS_MARKER)
  const reasonWithoutLogs = (
    logsStart >= 0 ? fullReason.slice(0, logsStart) : fullReason
  ).trim()

  const summary =
    reasonWithoutLogs.length > MAX_FAILURE_SUMMARY_LENGTH
      ? `${reasonWithoutLogs.slice(0, MAX_FAILURE_SUMMARY_LENGTH).trimEnd()}…`
      : reasonWithoutLogs

  return {
    fullReason,
    summary,
    hasMore: summary !== fullReason,
  }
}
