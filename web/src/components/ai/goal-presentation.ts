// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GoalResponse } from '@/api/client'

export type GoalTone = 'working' | 'good' | 'attention' | 'bad' | 'muted'

export interface GoalPresentation {
  label: string
  tone: GoalTone
  /** Still being observed by the server. */
  open: boolean
  canStop: boolean
  canRetry: boolean
}

/** What a goal's status means to the person reading the chat. */
export function presentGoal(
  goal: Pick<GoalResponse, 'status' | 'attempts' | 'max_attempts'>
): GoalPresentation {
  switch (goal.status) {
    case 'active':
      return {
        label: 'Watching',
        tone: 'working',
        open: true,
        canStop: true,
        canRetry: false,
      }
    case 'healing':
      return {
        label: `Fixing · attempt ${goal.attempts} of ${goal.max_attempts}`,
        tone: 'working',
        open: true,
        canStop: true,
        canRetry: false,
      }
    case 'needs_you':
      return {
        label: 'Needs you',
        tone: 'attention',
        open: true,
        canStop: true,
        canRetry: true,
      }
    case 'achieved':
      return {
        label: 'Done',
        tone: 'good',
        open: false,
        canStop: false,
        canRetry: false,
      }
    case 'failed':
      return {
        label: 'Gave up',
        tone: 'bad',
        open: false,
        canStop: false,
        canRetry: true,
      }
    case 'cancelled':
      return {
        label: 'Stopped',
        tone: 'muted',
        open: false,
        canStop: false,
        canRetry: false,
      }
    default:
      return {
        label: goal.status,
        tone: 'muted',
        open: false,
        canStop: false,
        canRetry: false,
      }
  }
}

/** Poll quickly while something is being watched, slowly otherwise. */
export function goalPollingInterval(
  goals: ReadonlyArray<
    Pick<GoalResponse, 'status' | 'attempts' | 'max_attempts'>
  >
): number {
  return goals.some((goal) => presentGoal(goal).open) ? 4000 : 15000
}

/**
 * Goals worth a row in the chat: everything still open, plus the latest few
 * that gave up (they can be retried). A goal that finished, or was stopped, has
 * nothing left to act on, so it leaves the list; a deployment it watched stays
 * available in the deployment panel.
 */
export function visibleGoals<
  T extends Pick<GoalResponse, 'status' | 'attempts' | 'max_attempts'>,
>(goals: readonly T[], failedToShow = 2): T[] {
  const open = goals.filter((goal) => presentGoal(goal).open)
  const failed = goals
    .filter((goal) => goal.status === 'failed')
    .slice(0, failedToShow)
  return [...open, ...failed]
}

export interface WatchedDeployment {
  projectId: number
  /** Absent when the goal follows the project's latest deployment. */
  deploymentId: number | null
  /** The goal that watched it, for its title and status. */
  goal: GoalResponse
}

/** Each deployment this chat has watched once, newest first. */
export function watchedDeployments(
  goals: readonly GoalResponse[]
): WatchedDeployment[] {
  const seen = new Set<string>()
  const found: WatchedDeployment[] = []
  const newestFirst = [...goals].sort((a, b) =>
    b.created_at.localeCompare(a.created_at)
  )
  for (const goal of newestFirst) {
    if (!goal.deployment) continue
    const key = `${goal.deployment.project_id}:${goal.deployment.deployment_id ?? 'latest'}`
    if (seen.has(key)) continue
    seen.add(key)
    found.push({
      projectId: goal.deployment.project_id,
      deploymentId: goal.deployment.deployment_id ?? null,
      goal,
    })
  }
  return found
}
