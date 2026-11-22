// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { GoalResponse } from '@/api/client'
import {
  goalPollingInterval,
  presentGoal,
  visibleGoals,
  watchedDeployments,
} from './goal-presentation'

const goal = (status: string, attempts = 0) => ({
  status,
  attempts,
  max_attempts: 3,
})

describe('presentGoal', () => {
  test('says how far a fix has got', () => {
    expect(presentGoal(goal('healing', 2)).label).toBe(
      'Fixing · attempt 2 of 3'
    )
  })

  test('only open goals can be stopped, only stuck ones retried', () => {
    expect(presentGoal(goal('active'))).toMatchObject({
      canStop: true,
      canRetry: false,
    })
    expect(presentGoal(goal('needs_you'))).toMatchObject({
      canStop: true,
      canRetry: true,
    })
    expect(presentGoal(goal('failed'))).toMatchObject({
      canStop: false,
      canRetry: true,
    })
    for (const status of ['achieved', 'cancelled']) {
      expect(presentGoal(goal(status))).toMatchObject({
        canStop: false,
        canRetry: false,
        open: false,
      })
    }
  })

  test('shows an unknown status as it is instead of hiding it', () => {
    expect(presentGoal(goal('paused')).label).toBe('paused')
  })
})

describe('goalPollingInterval', () => {
  test('polls faster while something is watched', () => {
    expect(goalPollingInterval([goal('active')])).toBe(4000)
    expect(goalPollingInterval([goal('achieved')])).toBe(15000)
    expect(goalPollingInterval([])).toBe(15000)
  })
})

describe('visibleGoals', () => {
  test('keeps open goals and ones that gave up, drops finished and stopped', () => {
    const goals = [
      goal('achieved'),
      goal('active'),
      goal('failed'),
      goal('cancelled'),
      goal('needs_you'),
    ]
    expect(visibleGoals(goals).map((g) => g.status)).toEqual([
      'active',
      'needs_you',
      'failed',
    ])
  })

  test('shows only the latest goals that gave up', () => {
    const goals = [goal('failed'), goal('failed'), goal('failed')]
    expect(visibleGoals(goals)).toHaveLength(2)
  })
})

describe('watchedDeployments', () => {
  const watching = (
    id: string,
    created_at: string,
    deployment?: { project_id: number; deployment_id?: number | null }
  ) =>
    ({
      ...goal('achieved'),
      id,
      created_at,
      deployment,
    }) as unknown as GoalResponse

  test('lists each deployment once, newest first, and skips other goals', () => {
    const found = watchedDeployments([
      watching('a', '2026-10-17T08:00:00Z', {
        project_id: 1,
        deployment_id: 10,
      }),
      watching('b', '2026-10-17T09:00:00Z', {
        project_id: 1,
        deployment_id: 11,
      }),
      watching('c', '2026-10-17T09:30:00Z'),
      watching('e', '2026-10-17T07:00:00Z', { project_id: 2 }),
      watching('d', '2026-10-17T10:00:00Z', {
        project_id: 1,
        deployment_id: 10,
      }),
    ])
    expect(found.map((d) => [d.deploymentId, d.goal.id])).toEqual([
      [10, 'd'],
      [11, 'b'],
      [null, 'e'],
    ])
  })
})
