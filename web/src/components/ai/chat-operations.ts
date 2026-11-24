// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ChatOperation,
  ChatOperationState,
  GoalResponse,
} from '@/api/client'
import {
  listChatOperationsOptions,
  listChatOperationsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery, type QueryClient } from '@tanstack/react-query'
import { presentGoal } from './goal-presentation'

/** Refresh quickly while something runs, so stages read as live. */
const RUNNING_POLL_MS = 3_000
/**
 * Otherwise check now and then: a turn can start something at any moment, and
 * the cards that start it refresh the list straight away.
 */
const IDLE_POLL_MS = 15_000
/** A failure stays on the button this long, then only in the list. */
const RECENT_FAILURE_MS = 60 * 60 * 1000

export function operationsPollingInterval(
  operations: readonly Pick<ChatOperation, 'state'>[]
): number {
  return operations.some((operation) => operation.state === 'running')
    ? RUNNING_POLL_MS
    : IDLE_POLL_MS
}

export function useChatOperations(conversationPublicId: string) {
  return useQuery({
    ...listChatOperationsOptions({ path: { public_id: conversationPublicId } }),
    refetchInterval: (query) =>
      operationsPollingInterval(query.state.data?.operations ?? []),
  })
}

/** Ask every open chat for its operations again, after a write ran. */
export function invalidateChatOperations(queryClient: QueryClient) {
  const sample = listChatOperationsQueryKey({ path: { public_id: '' } })[0]
  return queryClient.invalidateQueries({
    predicate: (query) => {
      const head = query.queryKey[0]
      return (
        typeof head === 'object' &&
        head !== null &&
        '_id' in head &&
        head._id === sample._id
      )
    },
  })
}

export type IndicatorState = 'idle' | 'working' | 'attention' | 'failed'

export interface IndicatorSummary {
  state: IndicatorState
  /** What the button says. Never empty: the button is always there. */
  label: string
}

/**
 * One line for the chat's operations button. A goal waiting on the user
 * outranks work in progress, which outranks a recent failure, so the button
 * never reads calm while something needs them.
 */
export function indicatorSummary(
  operations: readonly Pick<ChatOperation, 'state' | 'started_at'>[],
  goals: readonly Pick<GoalResponse, 'status' | 'attempts' | 'max_attempts'>[],
  now: number = Date.now()
): IndicatorSummary {
  if (goals.some((goal) => goal.status === 'needs_you')) {
    return { state: 'attention', label: 'Needs you' }
  }
  const running =
    operations.filter((operation) => operation.state === 'running').length +
    goals.filter(
      (goal) => presentGoal(goal).open && goal.status !== 'needs_you'
    ).length
  if (running > 0) return { state: 'working', label: `${running} running` }
  const failed =
    operations.filter(
      (operation) =>
        operation.state === 'failed' &&
        now - Date.parse(operation.started_at) < RECENT_FAILURE_MS
    ).length + goals.filter((goal) => goal.status === 'failed').length
  if (failed > 0) return { state: 'failed', label: `${failed} failed` }
  return { state: 'idle', label: 'Activity' }
}

const STATE_LABELS: Record<ChatOperationState, string> = {
  running: 'Running',
  succeeded: 'Done',
  failed: 'Failed',
  cancelled: 'Cancelled',
  not_started: 'Not started',
}

/** What a row says about where the operation is. */
export function operationStateLabel(
  operation: Pick<ChatOperation, 'state' | 'status'>
): string {
  if (operation.state !== 'running') return STATE_LABELS[operation.state]
  // `pending`/`queued` read better as waiting than as running.
  if (operation.status === 'pending' || operation.status === 'queued') {
    return 'Queued'
  }
  return operation.status ? 'Running' : 'Starting'
}
