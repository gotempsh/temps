// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const CONTAINER_LOGS_MARKER = 'Container logs for unhealthy/stopped services:'
const MAX_FAILURE_SUMMARY_LENGTH = 360

export interface DeploymentFailureSummary {
  fullReason: string
  summary: string
  hasMore: boolean
}

// Deployments that failed before the backend fix stored the job's message
// Rust-Debug-formatted (`Some("...")` with escaped quotes) and with the same
// prefixes repeated by every layer that wrapped it. Those rows are permanent,
// so normalise them here; reasons stored since then pass through unchanged.
const DEBUG_OPTION_WRAPPER = /Some\("([\s\S]*)"\)/
const REPEATED_PREFIXES = /Job execution failed: |Docker stream error: /g
const DOUBLED_BUILD_FAILED = /(Build failed: )+/g

function unescapeDebugString(value: string): string {
  return value.replace(/\\(["'\\])/g, '$1')
}

export function normalizeFailureReason(rawReason: string): string {
  const unwrapped = rawReason.replace(
    DEBUG_OPTION_WRAPPER,
    (_match, inner: string) => unescapeDebugString(inner)
  )
  const logsStart = unwrapped.indexOf(CONTAINER_LOGS_MARKER)
  const reason = logsStart >= 0 ? unwrapped.slice(0, logsStart) : unwrapped
  const logs = logsStart >= 0 ? unwrapped.slice(logsStart) : ''
  return (
    reason
      .replace(REPEATED_PREFIXES, '')
      .replace(DOUBLED_BUILD_FAILED, 'Build failed: ') + logs
  )
}

export function deploymentFailureSummary(
  rawReason: string
): DeploymentFailureSummary {
  const fullReason = normalizeFailureReason(rawReason)
    .replace(/\\n/g, '\n')
    .trim()
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
