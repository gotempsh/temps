// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ActivityCooldowns,
  ActivityErrorBody,
  ActivityOperation,
  ActivityStatus,
} from '@/api/client/types.gen'

export const OPERATION_LABEL: Record<ActivityOperation, string> = {
  website_analysis: 'Website analysis',
  preview: 'Activity preview',
  report: 'Activity report',
}

const FALLBACK_MESSAGE =
  'Could not complete the request. Check the AI provider settings and try again.'

/** The typed fields of an activity error response, when the error has them. */
export function activityProblem(
  error: unknown
): Partial<ActivityErrorBody> | null {
  if (!error || typeof error !== 'object') return null
  return error as Partial<ActivityErrorBody>
}

export function activityErrorMessage(error: unknown): string {
  const detail = activityProblem(error)?.detail
  return typeof detail === 'string' && detail ? detail : FALLBACK_MESSAGE
}

/**
 * A failure the operator has to act on. A running analysis or a cooldown is
 * shown from the polled status instead, as progress or a countdown.
 */
export function isActionableFailure(error: unknown): boolean {
  const code = activityProblem(error)?.code
  return code !== 'analysis_running' && code !== 'cooling_down'
}

export interface ActivityGate {
  /** The action running for this project, from the polled status. */
  running: { operation: ActivityOperation; startedAt: number } | null
  /** When `operation` may use the AI provider again, in epoch ms. */
  cooldownUntil: number | null
}

const COOLDOWN_FIELD: Record<ActivityOperation, keyof ActivityCooldowns> = {
  website_analysis: 'website_analysis_secs',
  preview: 'preview_secs',
  report: 'report_secs',
}

/**
 * Whether `operation` can start now. Any running action blocks the others:
 * the server runs one AI action per project at a time.
 */
export function activityGate(
  status: Pick<ActivityStatus, 'in_progress' | 'cooldowns'> | undefined,
  operation: ActivityOperation,
  statusUpdatedAt: number
): ActivityGate {
  const inProgress = status?.in_progress
  const cooldownSecs = status?.cooldowns?.[COOLDOWN_FIELD[operation]]
  return {
    running: inProgress
      ? {
          operation: inProgress.operation,
          startedAt: Date.parse(inProgress.started_at),
        }
      : null,
    cooldownUntil:
      typeof cooldownSecs === 'number' && cooldownSecs > 0
        ? statusUpdatedAt + cooldownSecs * 1000
        : null,
  }
}

/** Whole seconds left until `until`, never negative. */
export function secondsUntil(until: number | null, now: number): number {
  if (until == null) return 0
  return Math.max(0, Math.ceil((until - now) / 1000))
}

export function formatElapsed(startedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000))
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`
}
