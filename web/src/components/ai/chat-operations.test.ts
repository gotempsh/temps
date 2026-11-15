// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ChatOperationState } from '@/api/client'
import {
  indicatorSummary,
  operationStateLabel,
  operationsPollingInterval,
} from './chat-operations'

const NOW = Date.parse('2026-10-19T12:00:00Z')
const minutesAgo = (minutes: number) =>
  new Date(NOW - minutes * 60_000).toISOString()
const operation = (state: ChatOperationState, startedMinutesAgo = 1) => ({
  state,
  started_at: minutesAgo(startedMinutesAgo),
})
const goal = (status: string) => ({ status, attempts: 1, max_attempts: 3 })

describe('indicatorSummary', () => {
  test('the button is always there, quiet when nothing is going on', () => {
    expect(indicatorSummary([], [], NOW)).toEqual({
      state: 'idle',
      label: 'Activity',
    })
    expect(
      indicatorSummary(
        [operation('succeeded'), operation('cancelled')],
        [goal('achieved')],
        NOW
      ).state
    ).toBe('idle')
  })

  test('counts running operations and open goals together', () => {
    expect(
      indicatorSummary(
        [operation('running'), operation('succeeded')],
        [goal('active')],
        NOW
      )
    ).toEqual({ state: 'working', label: '2 running' })
  })

  test('a goal that needs the user outranks work in progress', () => {
    expect(
      indicatorSummary([operation('running')], [goal('needs_you')], NOW).state
    ).toBe('attention')
  })

  test('a recent failure shows when nothing else is running', () => {
    expect(indicatorSummary([operation('failed', 5)], [], NOW)).toEqual({
      state: 'failed',
      label: '1 failed',
    })
    expect(
      indicatorSummary([operation('failed'), operation('running')], [], NOW)
        .state
    ).toBe('working')
  })

  test('an old failure stays in the list but leaves the button calm', () => {
    expect(indicatorSummary([operation('failed', 120)], [], NOW).state).toBe(
      'idle'
    )
  })
})

describe('operationStateLabel', () => {
  test('a queued deployment reads as waiting, not running', () => {
    expect(operationStateLabel({ state: 'running', status: 'pending' })).toBe(
      'Queued'
    )
    expect(operationStateLabel({ state: 'running', status: 'running' })).toBe(
      'Running'
    )
  })

  test('a pipeline that has not created its deployment yet is starting', () => {
    expect(operationStateLabel({ state: 'running', status: null })).toBe(
      'Starting'
    )
  })

  test('finished states', () => {
    expect(
      operationStateLabel({ state: 'succeeded', status: 'completed' })
    ).toBe('Done')
    expect(operationStateLabel({ state: 'not_started', status: null })).toBe(
      'Not started'
    )
  })
})

describe('operationsPollingInterval', () => {
  test('refreshes quickly only while something runs', () => {
    expect(operationsPollingInterval([{ state: 'running' }])).toBe(3_000)
    expect(operationsPollingInterval([{ state: 'succeeded' }])).toBe(15_000)
    expect(operationsPollingInterval([])).toBe(15_000)
  })
})
