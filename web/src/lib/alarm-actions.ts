// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What an alarm row links to and which fix it offers. An alarm's scope is a
 * set of IDs (environment, deployment, service, container); these helpers
 * turn them into console links and pick the one-click remedy that matches
 * the alarm type, so the row answers "what fired, and what do I do now?".
 */

import type { AlarmResponse } from '@/api/client'

export type AlarmScopeLink = {
  label: string
  /** Console route; absent when the scope can't be resolved to a page. */
  href?: string
}

export type AlarmQuickAction =
  | {
      kind: 'restart_container'
      environmentId: number
      /** Docker container ID, as the container API expects. */
      containerId: string
      containerName: string
    }
  | { kind: 'redeploy'; deploymentId: number }
  | { kind: 'autofix'; href: string }

/** Alarm types raised by a container misbehaving at runtime. */
const CONTAINER_RUNTIME_ALARMS = new Set([
  'container_restart',
  'container_crash',
  'container_oom_killed',
])

/** Alarm types raised by a deployment that failed to go live. */
const DEPLOYMENT_ALARMS = new Set(['deployment_failed', 'health_check_failed'])

function metadataString(alarm: AlarmResponse, key: string): string | undefined {
  const metadata = alarm.metadata
  if (!metadata || typeof metadata !== 'object') return undefined
  const value = (metadata as Record<string, unknown>)[key]
  if (typeof value === 'string' && value.length > 0) return value
  if (typeof value === 'number' && Number.isFinite(value)) return String(value)
  return undefined
}

/**
 * Container alarms store the Docker container ID in `metadata.container_id`;
 * the top-level `container_id` is the internal row ID, which no page takes.
 */
function dockerContainerId(alarm: AlarmResponse): string | undefined {
  return metadataString(alarm, 'container_id')
}

/** Scope chips for an alarm, linked to their pages where one exists. */
export function alarmScopeLinks(
  alarm: AlarmResponse,
  projectSlug: string | undefined
): AlarmScopeLink[] {
  const links: AlarmScopeLink[] = []
  const projectBase = projectSlug ? `/projects/${projectSlug}` : undefined
  if (alarm.environment_id != null) {
    links.push({
      label: `env #${alarm.environment_id}`,
      href: projectBase
        ? `${projectBase}/environments?environment=${alarm.environment_id}`
        : undefined,
    })
  }
  if (alarm.deployment_id != null) {
    links.push({
      label: `deploy #${alarm.deployment_id}`,
      href: projectBase
        ? `${projectBase}/deployments/${alarm.deployment_id}`
        : undefined,
    })
  }
  if (alarm.service_id != null) {
    links.push({
      label: `service #${alarm.service_id}`,
      href: `/storage/${alarm.service_id}`,
    })
  }
  if (alarm.container_id != null) {
    const dockerId = dockerContainerId(alarm)
    const name = metadataString(alarm, 'container_name')
    links.push({
      label: name ?? `container #${alarm.container_id}`,
      href:
        projectBase && dockerId && alarm.environment_id != null
          ? `${projectBase}/environments/containers/${encodeURIComponent(dockerId)}?env=${alarm.environment_id}`
          : undefined,
    })
  }
  const groupId = metadataString(alarm, 'group_id')
  if (alarm.alarm_type === 'error_tracking_threshold' && groupId) {
    links.push({
      label: `error group #${groupId}`,
      href: projectBase ? `${projectBase}/errors/${groupId}` : undefined,
    })
  }
  return links.length > 0 ? links : [{ label: 'project-wide' }]
}

/**
 * The fix an alarm row offers, or `null` when its type has none (or the alarm
 * no longer needs one). Resolved alarms get no action: the problem is over.
 */
export function alarmQuickAction(
  alarm: AlarmResponse,
  projectSlug: string | undefined
): AlarmQuickAction | null {
  if (alarm.status === 'resolved') return null
  if (CONTAINER_RUNTIME_ALARMS.has(alarm.alarm_type)) {
    const containerId = dockerContainerId(alarm)
    if (!containerId || alarm.environment_id == null) return null
    return {
      kind: 'restart_container',
      environmentId: alarm.environment_id,
      containerId,
      containerName: metadataString(alarm, 'container_name') ?? containerId,
    }
  }
  if (DEPLOYMENT_ALARMS.has(alarm.alarm_type)) {
    return alarm.deployment_id != null
      ? { kind: 'redeploy', deploymentId: alarm.deployment_id }
      : null
  }
  if (alarm.alarm_type === 'error_tracking_threshold') {
    const groupId = metadataString(alarm, 'group_id')
    return groupId && projectSlug
      ? {
          kind: 'autofix',
          href: `/projects/${projectSlug}/errors/${groupId}/autofix`,
        }
      : null
  }
  return null
}
